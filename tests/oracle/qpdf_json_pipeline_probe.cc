#include <qpdf/Pipeline.hh>
#include <qpdf/Pl_Base64.hh>
#include <qpdf/Pl_Concatenate.hh>
#include <qpdf/Pl_OStream.hh>
#include <qpdf/Pl_String.hh>

#include <algorithm>
#include <exception>
#include <iomanip>
#include <iostream>
#include <sstream>
#include <stdexcept>
#include <streambuf>
#include <string>

namespace
{
    class RecordingPipeline: public Pipeline
    {
      public:
        RecordingPipeline() :
            Pipeline("recording", nullptr)
        {
        }

        void write(unsigned char const* data, size_t len) override
        {
            bytes.append(reinterpret_cast<char const*>(data), len);
        }

        void finish() override
        {
            ++finish_count;
        }

        std::string bytes;
        size_t finish_count{0};
    };

    class RejectingPipeline: public Pipeline
    {
      public:
        RejectingPipeline() :
            Pipeline("rejecting", nullptr)
        {
        }

        void write(unsigned char const* data, size_t len) override
        {
            bytes.append(reinterpret_cast<char const*>(data), len);
            throw std::runtime_error("downstream rejected chunk");
        }

        void finish() override
        {
        }

        std::string bytes;
    };

    class AcceptTwoThenEof: public std::streambuf
    {
      protected:
        std::streamsize xsputn(char const* data, std::streamsize len) override
        {
            auto const available =
                static_cast<std::streamsize>(2U - std::min<size_t>(bytes.size(), 2U));
            auto const accepted = std::min(len, available);
            bytes.append(data, static_cast<size_t>(accepted));
            return accepted;
        }

        int_type overflow(int_type ch) override
        {
            if (traits_type::eq_int_type(ch, traits_type::eof()) || bytes.size() >= 2U) {
                return traits_type::eof();
            }
            bytes.push_back(traits_type::to_char_type(ch));
            return ch;
        }

        int sync() override
        {
            ++finish_count;
            return 0;
        }

      public:
        std::string bytes;
        size_t finish_count{0};
    };

    std::string hex(std::string const& bytes)
    {
        std::ostringstream result;
        result << std::hex << std::setfill('0');
        for (unsigned char byte: bytes) {
            result << std::setw(2) << static_cast<unsigned int>(byte);
        }
        return result.str();
    }

    template <typename F>
    std::string status(F&& operation)
    {
        try {
            operation();
            return "ok";
        } catch (std::exception const& error) {
            return error.what();
        }
    }

    void emit(
        char const* case_name,
        std::string const& case_status,
        std::string const& bytes,
        size_t write_count,
        size_t finish_count)
    {
        std::cout << case_name << '\t' << case_status << '\t' << hex(bytes) << '\t'
                  << write_count << '\t' << finish_count << '\n';
    }

    void run_core()
    {
        {
            std::string bytes;
            Pl_String stage("string-null", nullptr, bytes);
            auto const case_status = status([&stage]() { stage.writeCStr("ab"); });
            emit("string-null", case_status, bytes, 1, 0);
        }

        {
            std::string bytes;
            RejectingPipeline rejecting;
            Pl_String stage("string-tee-error", &rejecting, bytes);
            auto const case_status = status([&stage]() { stage.writeCStr("ab"); });
            emit("string-tee-error", case_status, bytes, 1, 0);
        }

        {
            RecordingPipeline sink;
            Pl_Concatenate stage("concatenate-finish", &sink);
            auto const case_status = status([&stage]() {
                stage.writeCStr("one");
                stage.finish();
                stage.writeCStr("two");
                stage.manualFinish();
            });
            emit(
                "concatenate-finish",
                case_status,
                sink.bytes,
                2,
                sink.finish_count);
        }

        {
            RecordingPipeline sink;
            Pl_Base64 stage("base64", &sink, Pl_Base64::a_encode);
            auto const case_status = status([&stage]() {
                unsigned char const first[] = {0x00};
                unsigned char const second[] = {0xff, 0x10};
                unsigned char const third[] = {0x20};
                stage.write(first, sizeof(first));
                stage.write(second, sizeof(second));
                stage.write(third, sizeof(third));
                stage.finish();
            });
            emit(
                "base64-encode-split",
                case_status,
                sink.bytes,
                3,
                sink.finish_count);
        }

        {
            RecordingPipeline sink;
            Pl_Base64 stage("base64", &sink, Pl_Base64::a_decode);
            auto const case_status = status([&stage]() {
                stage.writeCStr("-_8=");
                stage.finish();
            });
            emit(
                "base64-decode-alias",
                case_status,
                sink.bytes,
                1,
                sink.finish_count);
        }

        {
            RecordingPipeline sink;
            Pl_Base64 stage("base64", &sink, Pl_Base64::a_decode);
            auto const case_status =
                status([&stage]() { stage.writeCStr("TQ==AAAA"); });
            emit(
                "base64-data-after-pad",
                case_status,
                sink.bytes,
                1,
                sink.finish_count);
        }

        {
            AcceptTwoThenEof stream_buffer;
            std::ostream stream(&stream_buffer);
            Pl_OStream stage("ostream-sticky", stream);
            auto const case_status = status([&stage]() {
                stage.writeCStr("abcd");
                stage.finish();
            });
            emit(
                "ostream-sticky",
                case_status,
                stream_buffer.bytes,
                1,
                stream_buffer.finish_count);
        }
    }
}

int main(int argc, char* argv[])
{
    if ((argc != 2) || (std::string(argv[1]) != "core")) {
        std::cerr << "qpdf_json_pipeline_probe: usage: core\n";
        return 2;
    }

    try {
        run_core();
        return 0;
    } catch (std::exception const& error) {
        std::cerr << "qpdf_json_pipeline_probe: " << error.what() << '\n';
        return 2;
    }
}
