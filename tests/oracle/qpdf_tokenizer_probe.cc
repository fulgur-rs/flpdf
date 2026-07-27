#include <qpdf/BufferInputSource.hh>
#include <qpdf/ContentNormalizer.hh>
#include <qpdf/Pl_Buffer.hh>
#include <qpdf/Pl_QPDFTokenizer.hh>
#include <qpdf/QPDF.hh>
#include <qpdf/QPDFObjectHandle.hh>
#include <qpdf/QPDFTokenizer.hh>

#include <cstdio>
#include <cstdlib>

#include <deque>
#include <iomanip>
#include <iostream>
#include <memory>
#include <optional>
#include <sstream>
#include <stdexcept>
#include <string>

namespace
{
    struct Options
    {
        std::string mode;
        std::string input;
        bool allow_eof{false};
        bool include_ignorable{false};
        bool allow_bad{false};
        size_t max_len{0};
        std::optional<size_t> inline_offset;
    };

    [[noreturn]] void
    usage(std::string const& message)
    {
        if (!message.empty()) {
            std::cerr << "qpdf_tokenizer_probe: " << message << "\n";
        }
        std::cerr
            << "usage: qpdf_tokenizer_probe"
               " --mode pull|push|pull-inline|push-inline|between|content|normalize"
               " --input-hex HEX --allow-eof 0|1 --include-ignorable 0|1"
               " --allow-bad 0|1 --max-len N --inline-offset none|N\n";
        std::exit(2);
    }

    unsigned char
    hex_nibble(char ch)
    {
        if (ch >= '0' && ch <= '9') {
            return static_cast<unsigned char>(ch - '0');
        }
        if (ch >= 'a' && ch <= 'f') {
            return static_cast<unsigned char>(ch - 'a' + 10);
        }
        if (ch >= 'A' && ch <= 'F') {
            return static_cast<unsigned char>(ch - 'A' + 10);
        }
        usage("invalid hex input");
    }

    std::string
    hex_decode(std::string const& value)
    {
        if ((value.size() % 2) != 0) {
            usage("hex input has odd length");
        }
        std::string result;
        result.reserve(value.size() / 2);
        for (size_t i = 0; i < value.size(); i += 2) {
            auto byte =
                static_cast<unsigned char>((hex_nibble(value.at(i)) << 4) |
                                           hex_nibble(value.at(i + 1)));
            result.push_back(static_cast<char>(byte));
        }
        return result;
    }

    std::string
    hex_encode(std::string const& value)
    {
        std::ostringstream result;
        result << std::hex << std::setfill('0');
        for (unsigned char byte: value) {
            result << std::setw(2) << static_cast<unsigned int>(byte);
        }
        return result.str();
    }

    bool
    parse_bool(std::string const& value, std::string const& flag)
    {
        if (value == "0") {
            return false;
        }
        if (value == "1") {
            return true;
        }
        usage(flag + " expects 0 or 1");
    }

    size_t
    parse_size(std::string const& value, std::string const& flag)
    {
        size_t consumed = 0;
        unsigned long long parsed = 0;
        try {
            parsed = std::stoull(value, &consumed);
        } catch (std::exception const&) {
            usage(flag + " expects a non-negative integer");
        }
        if (consumed != value.size()) {
            usage(flag + " expects a non-negative integer");
        }
        return static_cast<size_t>(parsed);
    }

    Options
    parse_options(int argc, char* argv[])
    {
        Options options;
        std::optional<std::string> input_hex;
        bool saw_allow_eof = false;
        bool saw_include_ignorable = false;
        bool saw_allow_bad = false;
        bool saw_max_len = false;
        bool saw_inline_offset = false;

        for (int i = 1; i < argc; ++i) {
            std::string flag = argv[i];
            if (i + 1 >= argc) {
                usage("missing value for " + flag);
            }
            std::string value = argv[++i];
            if (flag == "--mode") {
                options.mode = value;
            } else if (flag == "--input-hex") {
                input_hex = value;
            } else if (flag == "--allow-eof") {
                options.allow_eof = parse_bool(value, flag);
                saw_allow_eof = true;
            } else if (flag == "--include-ignorable") {
                options.include_ignorable = parse_bool(value, flag);
                saw_include_ignorable = true;
            } else if (flag == "--allow-bad") {
                options.allow_bad = parse_bool(value, flag);
                saw_allow_bad = true;
            } else if (flag == "--max-len") {
                options.max_len = parse_size(value, flag);
                saw_max_len = true;
            } else if (flag == "--inline-offset") {
                if (value != "none") {
                    options.inline_offset = parse_size(value, flag);
                }
                saw_inline_offset = true;
            } else {
                usage("unknown flag " + flag);
            }
        }

        if (options.mode.empty() || !input_hex || !saw_allow_eof ||
            !saw_include_ignorable || !saw_allow_bad || !saw_max_len ||
            !saw_inline_offset) {
            usage("all flags are required");
        }
        if (options.mode != "pull" && options.mode != "push" &&
            options.mode != "pull-inline" && options.mode != "push-inline" &&
            options.mode != "between" && options.mode != "content" &&
            options.mode != "normalize") {
            usage("invalid mode " + options.mode);
        }
        options.input = hex_decode(*input_hex);
        if (options.inline_offset && *options.inline_offset > options.input.size()) {
            usage("inline offset is beyond input");
        }
        bool inline_mode =
            options.mode == "pull-inline" || options.mode == "push-inline";
        if (inline_mode != options.inline_offset.has_value()) {
            usage("inline modes require --inline-offset and other modes require none");
        }
        if ((options.mode == "push" || options.mode == "push-inline" ||
             options.mode == "between") &&
            options.max_len != 0) {
            usage("max length is a pull-only QPDFTokenizer API");
        }
        return options;
    }

    char const*
    token_type_name(QPDFTokenizer::token_type_e type)
    {
        // Match qpdf/qpdf/test_tokenizer.cc:51-92. Keeping the exhaustive switch
        // makes a future qpdf enum addition a compiler-visible probe change.
        switch (type) {
        case QPDFTokenizer::tt_bad:
            return "bad";
        case QPDFTokenizer::tt_array_close:
            return "array_close";
        case QPDFTokenizer::tt_array_open:
            return "array_open";
        case QPDFTokenizer::tt_brace_close:
            return "brace_close";
        case QPDFTokenizer::tt_brace_open:
            return "brace_open";
        case QPDFTokenizer::tt_dict_close:
            return "dict_close";
        case QPDFTokenizer::tt_dict_open:
            return "dict_open";
        case QPDFTokenizer::tt_integer:
            return "integer";
        case QPDFTokenizer::tt_name:
            return "name";
        case QPDFTokenizer::tt_real:
            return "real";
        case QPDFTokenizer::tt_string:
            return "string";
        case QPDFTokenizer::tt_null:
            return "null";
        case QPDFTokenizer::tt_bool:
            return "bool";
        case QPDFTokenizer::tt_word:
            return "word";
        case QPDFTokenizer::tt_eof:
            return "eof";
        case QPDFTokenizer::tt_space:
            return "space";
        case QPDFTokenizer::tt_comment:
            return "comment";
        case QPDFTokenizer::tt_inline_image:
            return "inline-image";
        }
        throw std::logic_error("unknown QPDFTokenizer token type");
    }

    void
    configure(QPDFTokenizer& tokenizer, Options const& options)
    {
        if (options.allow_eof) {
            tokenizer.allowEOF();
        }
        if (options.include_ignorable) {
            tokenizer.includeIgnorable();
        }
    }

    void
    emit_token(
        QPDFTokenizer::Token const& token,
        std::optional<qpdf_offset_t> start,
        std::optional<qpdf_offset_t> end,
        std::optional<char> unread)
    {
        std::cout << token_type_name(token.getType()) << '\t'
                  << hex_encode(token.getValue()) << '\t'
                  << hex_encode(token.getRawValue()) << '\t'
                  << hex_encode(token.getErrorMessage()) << '\t';
        if (start) {
            std::cout << *start;
        } else {
            std::cout << '-';
        }
        std::cout << '\t';
        if (end) {
            std::cout << *end;
        } else {
            std::cout << '-';
        }
        std::cout << '\t';
        if (unread) {
            std::cout << hex_encode(std::string(1, *unread));
        }
        std::cout << '\n';
    }

    std::shared_ptr<BufferInputSource>
    make_input(Options const& options)
    {
        return std::make_shared<BufferInputSource>(
            "qpdf-tokenizer-probe", options.input);
    }

    void
    dump_pull(Options const& options, bool inline_mode)
    {
        auto input = make_input(options);
        QPDFTokenizer tokenizer;
        configure(tokenizer, options);
        if (inline_mode) {
            input->seek(
                static_cast<qpdf_offset_t>(*options.inline_offset), SEEK_SET);
            tokenizer.expectInlineImage(input);
        }

        for (size_t count = 0; count < options.input.size() + 32; ++count) {
            auto token = tokenizer.readToken(
                input, "qpdf tokenizer probe", options.allow_bad, options.max_len);
            auto start = input->getLastOffset();
            auto end = input->tell();
            emit_token(token, start, end, std::nullopt);
            if (token.getType() == QPDFTokenizer::tt_eof ||
                (!options.allow_eof &&
                 token.getType() == QPDFTokenizer::tt_bad &&
                 token.getRawValue().empty() &&
                 static_cast<size_t>(input->tell()) == options.input.size())) {
                return;
            }
        }
        throw std::runtime_error("pull case did not terminate");
    }

    std::deque<char>
    push_bytes(Options const& options, bool inline_mode)
    {
        auto start = inline_mode ? *options.inline_offset : 0;
        return std::deque<char>(
            options.input.begin() + static_cast<std::ptrdiff_t>(start),
            options.input.end());
    }

    void
    enter_inline_image(
        QPDFTokenizer& tokenizer, Options const& options)
    {
        auto input = make_input(options);
        input->seek(
            static_cast<qpdf_offset_t>(*options.inline_offset), SEEK_SET);
        tokenizer.expectInlineImage(input);
    }

    void
    dump_push(Options const& options, bool inline_mode)
    {
        QPDFTokenizer tokenizer;
        configure(tokenizer, options);
        if (inline_mode) {
            enter_inline_image(tokenizer, options);
        }
        auto pending = push_bytes(options, inline_mode);

        while (!pending.empty()) {
            char byte = pending.front();
            pending.pop_front();
            tokenizer.presentCharacter(byte);
            QPDFTokenizer::Token token;
            bool unread = false;
            char unread_byte = '\0';
            if (tokenizer.getToken(token, unread, unread_byte)) {
                if (unread) {
                    pending.push_front(unread_byte);
                }
                emit_token(
                    token,
                    std::nullopt,
                    std::nullopt,
                    unread ? std::optional<char>(unread_byte) : std::nullopt);
            }
        }

        for (int count = 0; count < 4; ++count) {
            tokenizer.presentEOF();
            QPDFTokenizer::Token token;
            bool unread = false;
            char unread_byte = '\0';
            if (!tokenizer.getToken(token, unread, unread_byte)) {
                throw std::runtime_error("presentEOF did not finish a token");
            }
            emit_token(
                token,
                std::nullopt,
                std::nullopt,
                unread ? std::optional<char>(unread_byte) : std::nullopt);
            if (token.getType() == QPDFTokenizer::tt_eof ||
                token.getType() == QPDFTokenizer::tt_bad) {
                return;
            }
        }
        throw std::runtime_error("push case did not terminate");
    }

    void
    dump_between(Options const& options)
    {
        QPDFTokenizer tokenizer;
        configure(tokenizer, options);
        auto pending = push_bytes(options, false);
        size_t event = 0;

        while (!pending.empty()) {
            char byte = pending.front();
            pending.pop_front();
            bool before = tokenizer.betweenTokens();
            tokenizer.presentCharacter(byte);
            bool after_present = tokenizer.betweenTokens();
            QPDFTokenizer::Token token;
            bool unread = false;
            char unread_byte = '\0';
            bool ready = tokenizer.getToken(token, unread, unread_byte);
            bool after_get = tokenizer.betweenTokens();
            if (unread) {
                pending.push_front(unread_byte);
            }
            std::cout << "state\t" << event << '\t'
                      << hex_encode(std::string(1, byte)) << '\t'
                      << static_cast<int>(before) << '\t'
                      << static_cast<int>(after_present) << '\t'
                      << static_cast<int>(ready) << '\t';
            if (unread) {
                std::cout << hex_encode(std::string(1, unread_byte));
            }
            std::cout << "\nreset\t" << event << '\t'
                      << static_cast<int>(after_get) << '\n';
            ++event;
        }
    }

    class ContentCallbacks final: public QPDFObjectHandle::ParserCallbacks
    {
      public:
        void
        handleObject(QPDFObjectHandle object, size_t offset, size_t length) override
        {
            std::cout << offset << '\t' << length << '\t'
                      << object.getTypeName() << '\t' << object.unparse() << '\n';
        }

        void
        handleEOF() override
        {
            std::cout << "eof\n";
        }
    };

    void
    dump_content(Options const& options)
    {
        QPDF qpdf;
        qpdf.emptyPDF();
        auto stream = qpdf.newStream(options.input);
        ContentCallbacks callbacks;
        stream.parseAsContents(&callbacks);
    }

    void
    dump_normalize(Options const& options)
    {
        Pl_Buffer output("content normalizer output");
        ContentNormalizer normalizer;
        Pl_QPDFTokenizer tokenizer("content normalizer", &normalizer, &output);
        tokenizer.write(
            reinterpret_cast<unsigned char const*>(options.input.data()),
            options.input.size());
        tokenizer.finish();
        std::cout << "output\t" << hex_encode(output.getString()) << '\n'
                  << "any_bad_tokens\t" << static_cast<int>(normalizer.anyBadTokens()) << '\n'
                  << "last_token_was_bad\t"
                  << static_cast<int>(normalizer.lastTokenWasBad()) << '\n';
    }
} // namespace

int
main(int argc, char* argv[])
{
    try {
        auto options = parse_options(argc, argv);
        if (options.mode == "pull") {
            dump_pull(options, false);
        } else if (options.mode == "pull-inline") {
            dump_pull(options, true);
        } else if (options.mode == "push") {
            dump_push(options, false);
        } else if (options.mode == "push-inline") {
            dump_push(options, true);
        } else if (options.mode == "content") {
            dump_content(options);
        } else if (options.mode == "normalize") {
            dump_normalize(options);
        } else {
            dump_between(options);
        }
        return 0;
    } catch (std::exception const& error) {
        std::cerr << "qpdf_tokenizer_probe: " << error.what() << '\n';
        return 1;
    }
}
