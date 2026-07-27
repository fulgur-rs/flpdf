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
            auto byte = std::stoul(value.substr(i, 2), nullptr, 16);
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
}

int
main(int argc, char* argv[])
{
    try {
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
        size_t split_at = std::stoull(argv[4]);
        if (split_at > input.size()) {
            throw std::runtime_error("split exceeds input");
        }
        if (key.empty()) {
            throw std::runtime_error("empty explicit key");
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
