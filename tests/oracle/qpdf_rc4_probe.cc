#include <qpdf/Pl_RC4.hh>
#include <qpdf/RC4_native.hh>

#include <cstdlib>
#include <iomanip>
#include <iostream>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

namespace
{
    std::vector<unsigned char>
    decode(std::string const& value)
    {
        if ((value.size() % 2) != 0) {
            throw std::runtime_error("odd-length hex");
        }
        std::vector<unsigned char> result;
        result.reserve(value.size() / 2 + 1);
        for (size_t i = 0; i < value.size(); i += 2) {
            auto const encoded_byte = value.substr(i, 2);
            size_t consumed = 0;
            auto byte = std::stoul(encoded_byte, &consumed, 16);
            if (consumed != encoded_byte.size()) {
                throw std::runtime_error("invalid hex");
            }
            result.push_back(static_cast<unsigned char>(byte));
        }
        return result;
    }

    std::string
    encode(std::vector<unsigned char> const& value)
    {
        std::ostringstream result;
        result << std::hex << std::setfill('0');
        for (auto byte: value) {
            result << std::setw(2) << static_cast<unsigned int>(byte);
        }
        return result.str();
    }

    RC4_native
    make_cipher(
        std::vector<unsigned char> const& key,
        size_t explicit_key_len,
        bool c_string)
    {
        return RC4_native(
            key.data(),
            c_string ? -1 : static_cast<int>(explicit_key_len));
    }

    size_t
    parse_size(char const* value, char const* label)
    {
        size_t consumed = 0;
        auto result = std::stoull(value, &consumed);
        if (consumed != std::string(value).size()) {
            throw std::runtime_error(std::string("invalid ") + label);
        }
        return static_cast<size_t>(result);
    }

    class RecordingPipeline: public Pipeline
    {
      public:
        RecordingPipeline() :
            Pipeline("recording", nullptr)
        {
        }

        void write(unsigned char const* data, size_t len) override
        {
            chunks.push_back(len);
            output.insert(output.end(), data, data + len);
        }

        void finish() override
        {
            ++finishes;
        }

        std::vector<unsigned char> output;
        std::vector<size_t> chunks;
        size_t finishes{0};
    };

    int
    run_pipeline(int argc, char* argv[])
    {
        if (argc != 7) {
            throw std::runtime_error(
                "usage: qpdf_rc4_probe pipeline explicit|cstr KEY_HEX "
                "INPUT_LEN WRITE_SPLIT OUT_BUFFER_SIZE");
        }
        bool c_string = std::string(argv[2]) == "cstr";
        if (!c_string && std::string(argv[2]) != "explicit") {
            throw std::runtime_error("invalid key mode");
        }
        auto key = decode(argv[3]);
        if (key.empty()) {
            throw std::runtime_error(
                c_string ? "empty C-string key" : "empty explicit key");
        }
        if (c_string && key.front() == 0) {
            throw std::runtime_error("empty C-string key");
        }
        auto input_len = parse_size(argv[4], "input length");
        auto write_split = parse_size(argv[5], "write split");
        auto out_buffer_size = parse_size(argv[6], "output buffer size");
        if (write_split > input_len) {
            throw std::runtime_error("write split exceeds input");
        }
        if (out_buffer_size == 0) {
            throw std::runtime_error("zero output buffer size");
        }

        std::vector<unsigned char> input;
        input.reserve(input_len);
        for (size_t i = 0; i < input_len; ++i) {
            input.push_back(static_cast<unsigned char>((i * 37U + 11U) & 0xffU));
        }
        unsigned char empty_input_sentinel = 0;
        auto input_data = input.empty() ? &empty_input_sentinel : input.data();
        auto explicit_key_len = key.size();
        key.push_back(0);

        RecordingPipeline sink;
        Pl_RC4 stage(
            "pl-rc4",
            &sink,
            key.data(),
            c_string ? -1 : static_cast<int>(explicit_key_len),
            out_buffer_size);
        stage.write(input_data, write_split);
        stage.write(input_data + write_split, input_len - write_split);
        stage.finish();
        stage.finish();

        std::string after_finish;
        try {
            unsigned char byte = 0;
            stage.write(&byte, 1);
        } catch (std::logic_error const& error) {
            after_finish = error.what();
        }
        if (after_finish.empty()) {
            throw std::runtime_error("write after finish did not fail");
        }

        std::ostringstream chunks;
        for (size_t i = 0; i < sink.chunks.size(); ++i) {
            if (i > 0) {
                chunks << ",";
            }
            chunks << sink.chunks.at(i);
        }
        std::cout << "output\t" << encode(sink.output) << "\n"
                  << "chunks\t" << chunks.str() << "\n"
                  << "finishes\t" << sink.finishes << "\n"
                  << "after-finish\t" << after_finish << "\n";
        return 0;
    }
}

int
main(int argc, char* argv[])
{
    try {
        if ((argc >= 2) && (std::string(argv[1]) == "pipeline")) {
            return run_pipeline(argc, argv);
        }
        if (argc != 5) {
            throw std::runtime_error(
                "usage: qpdf_rc4_probe explicit|cstr KEY_HEX INPUT_HEX SPLIT");
        }
        bool c_string = std::string(argv[1]) == "cstr";
        if (!c_string && std::string(argv[1]) != "explicit") {
            throw std::runtime_error("invalid key mode");
        }
        auto key = decode(argv[2]);
        auto input = decode(argv[3]);
        size_t consumed = 0;
        size_t split_at = std::stoull(argv[4], &consumed);
        if (consumed != std::string(argv[4]).size()) {
            throw std::runtime_error("invalid split");
        }
        if (split_at > input.size()) {
            throw std::runtime_error("split exceeds input");
        }
        if (key.empty()) {
            throw std::runtime_error(
                c_string ? "empty C-string key" : "empty explicit key");
        }
        if (c_string && key.front() == 0) {
            throw std::runtime_error("empty C-string key");
        }
        size_t explicit_key_len = key.size();
        size_t input_len = input.size();
        key.push_back(0);
        input.push_back(0);

        auto one_cipher = make_cipher(key, explicit_key_len, c_string);
        std::vector<unsigned char> one(input_len == 0 ? 1 : input_len);
        one_cipher.process(input.data(), input_len, one.data());
        one.resize(input_len);

        auto split_cipher = make_cipher(key, explicit_key_len, c_string);
        std::vector<unsigned char> split(input_len == 0 ? 1 : input_len);
        split_cipher.process(input.data(), split_at, split.data());
        split_cipher.process(
            input.data() + split_at,
            input_len - split_at,
            split.data() + split_at);
        split.resize(input_len);

        auto in_place_cipher = make_cipher(key, explicit_key_len, c_string);
        auto in_place = input;
        in_place_cipher.process(
            in_place.data(), input_len, in_place.data());
        in_place.resize(input_len);

        std::cout << "one\t" << encode(one) << "\n"
                  << "split\t" << encode(split) << "\n"
                  << "in-place\t" << encode(in_place) << "\n";
        return 0;
    } catch (std::exception const& error) {
        std::cerr << "qpdf_rc4_probe: " << error.what() << "\n";
        return 2;
    }
}
