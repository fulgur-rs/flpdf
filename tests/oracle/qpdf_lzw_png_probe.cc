// Live qpdf 11.9.0 oracle probe for Pl_LZWDecoder and Pl_PNGFilter.
//
// The probe constructs one component directly from the pinned qpdf source,
// drives it through a scripted operation sequence against an instrumented
// downstream Pipeline, and reports every observable: downstream call
// boundaries, accumulated output, and the category and exact text of any
// exception raised by construction or by an individual operation.

#include <qpdf/Pl_LZWDecoder.hh>
#include <qpdf/Pl_PNGFilter.hh>

#include <cctype>
#include <cstring>
#include <iomanip>
#include <iostream>
#include <limits>
#include <memory>
#include <set>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

namespace
{
    struct Call
    {
        std::string kind;
        std::vector<unsigned char> data;
        bool failed;
    };

    class RecordingPipeline: public Pipeline
    {
      public:
        RecordingPipeline(std::set<size_t> fail_writes, std::set<size_t> fail_finishes) :
            Pipeline("recording", nullptr),
            fail_writes(std::move(fail_writes)),
            fail_finishes(std::move(fail_finishes))
        {
        }

        void
        write(unsigned char const* data, size_t len) override
        {
            auto const failed =
                this->fail_writes.find(++this->write_count) != this->fail_writes.end();
            std::vector<unsigned char> recorded;
            if (len != 0) {
                recorded.assign(data, data + len);
            }
            calls.push_back(Call{"write", std::move(recorded), failed});
            if (failed) {
                throw std::runtime_error("sink write failure " + std::to_string(this->write_count));
            }
            if (len != 0) {
                output.insert(output.end(), data, data + len);
            }
        }

        void
        finish() override
        {
            auto const failed =
                this->fail_finishes.find(++this->finish_count) != this->fail_finishes.end();
            calls.push_back(Call{"finish", {}, failed});
            if (failed) {
                throw std::runtime_error(
                    "sink finish failure " + std::to_string(this->finish_count));
            }
        }

        std::vector<Call> calls;
        std::vector<unsigned char> output;

      private:
        std::set<size_t> fail_writes;
        std::set<size_t> fail_finishes;
        size_t write_count{0};
        size_t finish_count{0};
    };

    class ArgumentError: public std::runtime_error
    {
      public:
        using std::runtime_error::runtime_error;
    };

    std::string
    hex(std::vector<unsigned char> const& bytes)
    {
        std::ostringstream result;
        result << std::hex << std::setfill('0');
        for (auto byte: bytes) {
            result << std::setw(2) << static_cast<unsigned int>(byte);
        }
        return result.str();
    }

    std::string
    hex(std::string const& value)
    {
        return hex(std::vector<unsigned char>(value.begin(), value.end()));
    }

    int
    hex_nibble(char value)
    {
        if ((value >= '0') && (value <= '9')) {
            return value - '0';
        }
        value = static_cast<char>(std::tolower(static_cast<unsigned char>(value)));
        if ((value >= 'a') && (value <= 'f')) {
            return value - 'a' + 10;
        }
        return -1;
    }

    std::vector<unsigned char>
    decode_hex(std::string const& value)
    {
        if ((value.size() % 2) != 0) {
            throw ArgumentError("malformed hex");
        }
        std::vector<unsigned char> result;
        result.reserve(value.size() / 2);
        for (size_t i = 0; i < value.size(); i += 2) {
            auto const high = hex_nibble(value.at(i));
            auto const low = hex_nibble(value.at(i + 1));
            if ((high < 0) || (low < 0)) {
                throw ArgumentError("malformed hex");
            }
            result.push_back(static_cast<unsigned char>((high << 4) | low));
        }
        return result;
    }

    std::set<size_t>
    parse_failure_list(std::string const& value)
    {
        if (value == "-") {
            return {};
        }
        if (value.empty()) {
            throw ArgumentError("malformed failure list");
        }

        std::set<size_t> result;
        size_t start = 0;
        while (start < value.size()) {
            auto const end = value.find(',', start);
            auto const item = value.substr(start, end - start);
            if (item.empty() || ((item.size() > 1) && (item.front() == '0'))) {
                throw ArgumentError("malformed failure list");
            }
            size_t number = 0;
            for (auto character: item) {
                if (!std::isdigit(static_cast<unsigned char>(character))) {
                    throw ArgumentError("malformed failure list");
                }
                auto const digit = static_cast<size_t>(character - '0');
                if (number > ((std::numeric_limits<size_t>::max() - digit) / 10)) {
                    throw ArgumentError("malformed failure list");
                }
                number = number * 10 + digit;
            }
            if (number == 0) {
                throw ArgumentError("malformed failure list");
            }
            result.insert(number);
            if (end == std::string::npos) {
                break;
            }
            if (end + 1 == value.size()) {
                throw ArgumentError("malformed failure list");
            }
            start = end + 1;
        }
        return result;
    }

    // Parses a decimal value in the inclusive range 0 .. 2^32-1. The PNG
    // component takes unsigned int geometry, so the probe must be able to
    // request values that wrap qpdf's 32-bit row-width arithmetic.
    unsigned int
    parse_u32(std::string const& value)
    {
        if (value.empty() || ((value.size() > 1) && (value.front() == '0'))) {
            throw ArgumentError("malformed unsigned value");
        }
        unsigned long long number = 0;
        for (auto character: value) {
            if (!std::isdigit(static_cast<unsigned char>(character))) {
                throw ArgumentError("malformed unsigned value");
            }
            number = number * 10 + static_cast<unsigned long long>(character - '0');
            if (number > 0xffffffffULL) {
                throw ArgumentError("unsigned value out of range");
            }
        }
        return static_cast<unsigned int>(number);
    }

    std::vector<std::string>
    split(std::string const& value, char separator)
    {
        std::vector<std::string> result;
        size_t start = 0;
        while (true) {
            auto const end = value.find(separator, start);
            if (end == std::string::npos) {
                result.push_back(value.substr(start));
                return result;
            }
            result.push_back(value.substr(start, end - start));
            start = end + 1;
        }
    }

    enum class OperationKind { write, finish };

    struct Operation
    {
        OperationKind kind;
        std::vector<unsigned char> data;
    };

    Operation
    parse_operation(std::string const& value)
    {
        if (value == "f") {
            return {OperationKind::finish, {}};
        }
        if (value.rfind("w:", 0) == 0) {
            return {OperationKind::write, decode_hex(value.substr(2))};
        }
        throw ArgumentError("malformed operation");
    }

    // Codec selectors:
    //   lzw:0 / lzw:1                 early code change disabled / enabled
    //   png-decode:COLUMNS,COLORS,BITS
    //   png-encode:COLUMNS,COLORS,BITS
    std::unique_ptr<Pipeline>
    make_codec(std::string const& codec, RecordingPipeline* sink)
    {
        auto const separator = codec.find(':');
        if (separator == std::string::npos) {
            throw ArgumentError("unknown codec");
        }
        auto const name = codec.substr(0, separator);
        auto const parameters = codec.substr(separator + 1);

        if (name == "lzw") {
            if ((parameters != "0") && (parameters != "1")) {
                throw ArgumentError("malformed lzw parameters");
            }
            return std::make_unique<Pl_LZWDecoder>("oracle codec", sink, parameters == "1");
        }

        if ((name == "png-decode") || (name == "png-encode")) {
            auto const fields = split(parameters, ',');
            if (fields.size() != 3) {
                throw ArgumentError("malformed png parameters");
            }
            auto const action =
                (name == "png-decode") ? Pl_PNGFilter::a_decode : Pl_PNGFilter::a_encode;
            return std::make_unique<Pl_PNGFilter>(
                "oracle codec",
                sink,
                action,
                parse_u32(fields.at(0)),
                parse_u32(fields.at(1)),
                parse_u32(fields.at(2)));
        }

        throw ArgumentError("unknown codec");
    }

    void
    emit_result(std::string const& record, size_t index, std::string const& category, std::string const& detail)
    {
        std::cout << record << "\t" << index << "\t" << category << "\t" << detail << "\n";
    }
} // namespace

int
main(int argc, char* argv[])
{
    try {
        if (argc < 5) {
            throw ArgumentError("usage: qpdf_lzw_png_probe CODEC FAIL_WRITES FAIL_FINISHES OP...");
        }
        auto const fail_writes = parse_failure_list(argv[2]);
        auto const fail_finishes = parse_failure_list(argv[3]);
        std::vector<Operation> operations;
        operations.reserve(static_cast<size_t>(argc - 4));
        for (int i = 4; i < argc; ++i) {
            operations.push_back(parse_operation(argv[i]));
        }

        RecordingPipeline sink(fail_writes, fail_finishes);
        std::unique_ptr<Pipeline> codec;
        try {
            codec = make_codec(argv[1], &sink);
        } catch (ArgumentError const&) {
            throw;
        } catch (std::logic_error const& error) {
            emit_result("ctor", 0, "logic", hex(std::string(error.what())));
            return 0;
        } catch (std::runtime_error const& error) {
            emit_result("ctor", 0, "runtime", hex(std::string(error.what())));
            return 0;
        }
        emit_result("ctor", 0, "ok", "");

        for (size_t i = 0; i < operations.size(); ++i) {
            auto const& operation = operations.at(i);
            try {
                if (operation.kind == OperationKind::write) {
                    auto const* data = operation.data.empty() ? nullptr : operation.data.data();
                    codec->write(data, operation.data.size());
                } else {
                    codec->finish();
                }
                emit_result("op", i, "ok", "");
            } catch (std::logic_error const& error) {
                emit_result("op", i, "logic", hex(std::string(error.what())));
            } catch (std::runtime_error const& error) {
                emit_result("op", i, "runtime", hex(std::string(error.what())));
            }
        }
        for (auto const& call: sink.calls) {
            std::cout << "call\t" << call.kind << "\t" << (call.failed ? 1 : 0) << "\t"
                      << call.data.size() << "\t" << hex(call.data) << "\n";
        }
        std::cout << "output\t" << hex(sink.output) << "\n";
        return 0;
    } catch (ArgumentError const& error) {
        std::cerr << "qpdf_lzw_png_probe: " << error.what() << "\n";
        return 2;
    } catch (std::exception const& error) {
        std::cerr << "qpdf_lzw_png_probe: " << error.what() << "\n";
        return 2;
    }
}
