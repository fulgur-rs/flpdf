#if defined(__linux__) && !defined(_GNU_SOURCE)
# define _GNU_SOURCE
#endif

#include <qpdf/Pipeline.hh>
#include <qpdf/Pl_Base64.hh>
#include <qpdf/Pl_Concatenate.hh>
#include <qpdf/Pl_OStream.hh>
#include <qpdf/Pl_StdioFile.hh>
#include <qpdf/Pl_String.hh>

#include <algorithm>
#include <array>
#include <cerrno>
#include <cstdio>
#include <deque>
#include <exception>
#include <iomanip>
#include <iostream>
#include <sstream>
#include <stdexcept>
#include <streambuf>
#include <string>
#include <vector>

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

#ifdef __linux__
    enum class WriteStepKind
    {
        accept,
        interrupted,
        zero,
        error
    };

    struct WriteStep
    {
        WriteStepKind kind;
        size_t count{0};
        int error{0};
    };

    enum class CookiePhase
    {
        none,
        write,
        finish,
        close
    };

    struct Cookie
    {
        std::vector<unsigned char> bytes;
        std::deque<WriteStep> steps;
        CookiePhase phase{CookiePhase::none};
        size_t pipeline_write_count{0};
        size_t pipeline_finish_count{0};
        std::vector<size_t> write_lengths;
        std::vector<size_t> finish_lengths;
        std::vector<size_t> close_lengths;
        bool capture_bytes{true};
        bool unexpected_phase{false};
    };

    ssize_t cookie_write(void* opaque, char const* data, size_t len)
    {
        auto& cookie = *static_cast<Cookie*>(opaque);
        if (cookie.phase == CookiePhase::write) {
            cookie.write_lengths.push_back(len);
        } else if (cookie.phase == CookiePhase::finish) {
            cookie.finish_lengths.push_back(len);
        } else if (cookie.phase == CookiePhase::close) {
            cookie.close_lengths.push_back(len);
        } else {
            cookie.unexpected_phase = true;
            errno = EIO;
            return -1;
        }
        WriteStep step{WriteStepKind::accept, len, 0};
        if (!cookie.steps.empty()) {
            step = cookie.steps.front();
            cookie.steps.pop_front();
        }

        switch (step.kind) {
        case WriteStepKind::accept: {
            auto const accepted = std::min(step.count, len);
            if (cookie.capture_bytes) {
                cookie.bytes.insert(
                    cookie.bytes.end(),
                    reinterpret_cast<unsigned char const*>(data),
                    reinterpret_cast<unsigned char const*>(data) + accepted);
            }
            return static_cast<ssize_t>(accepted);
        }
        case WriteStepKind::interrupted:
            errno = EINTR;
            return -1;
        case WriteStepKind::zero:
            errno = step.error;
            return 0;
        case WriteStepKind::error:
            errno = step.error;
            return -1;
        }
        throw std::logic_error("unreachable cookie write step");
    }

    template <typename F>
    void during(Cookie& cookie, CookiePhase phase, F&& operation)
    {
        cookie.phase = phase;
        if (phase == CookiePhase::write) {
            ++cookie.pipeline_write_count;
        } else if (phase == CookiePhase::finish) {
            ++cookie.pipeline_finish_count;
        }
        try {
            operation();
            cookie.phase = CookiePhase::none;
        } catch (...) {
            cookie.phase = CookiePhase::none;
            throw;
        }
    }

    void verify_cookie_lifecycle(
        char const* case_name,
        Cookie const& cookie,
        std::vector<size_t> const& write_lengths,
        std::vector<size_t> const& finish_lengths,
        std::vector<size_t> const& close_lengths)
    {
        if (cookie.unexpected_phase || !cookie.steps.empty() ||
            (cookie.write_lengths != write_lengths) ||
            (cookie.finish_lengths != finish_lengths) ||
            (cookie.close_lengths != close_lengths)) {
            throw std::runtime_error(
                std::string(case_name) + ": cookie lifecycle mismatch");
        }
    }

    void close_cookie(FILE* file, Cookie& cookie)
    {
        cookie.phase = CookiePhase::close;
        auto const close_status = fclose(file);
        cookie.phase = CookiePhase::none;
        if (close_status != 0) {
            throw std::runtime_error("fclose failed");
        }
    }

    FILE* open_cookie(Cookie& cookie, std::array<char, 4096>& buffer)
    {
        cookie_io_functions_t io{
            .read = nullptr,
            .write = cookie_write,
            .seek = nullptr,
            .close = nullptr,
        };
        auto* file = fopencookie(&cookie, "wb", io);
        if (file == nullptr) {
            throw std::runtime_error("fopencookie failed");
        }
        if (setvbuf(file, buffer.data(), _IOFBF, buffer.size()) != 0) {
            fclose(file);
            throw std::runtime_error("setvbuf failed");
        }
        return file;
    }

    FILE* open_buffered(char const* path)
    {
        auto* file = fopen(path, "w+b");
        if (file == nullptr) {
            throw std::runtime_error(std::string("fopen failed: ") + path);
        }
        if (setvbuf(file, nullptr, _IOFBF, 4096) != 0) {
            fclose(file);
            throw std::runtime_error("setvbuf failed");
        }
        return file;
    }

    std::string read_regular_file(FILE* file)
    {
        if (fseek(file, 0, SEEK_SET) != 0) {
            throw std::runtime_error("fseek failed");
        }
        std::string bytes;
        char buffer[4096];
        while (auto const count = fread(buffer, 1, sizeof(buffer), file)) {
            bytes.append(buffer, count);
        }
        if (ferror(file)) {
            throw std::runtime_error("fread failed");
        }
        return bytes;
    }

    std::vector<unsigned char> patterned_bytes(size_t count)
    {
        std::vector<unsigned char> result;
        result.reserve(count);
        for (size_t index = 0; index < count; ++index) {
            result.push_back(static_cast<unsigned char>(index % 251));
        }
        return result;
    }

    void run_stdio()
    {
        {
            auto* file = open_buffered("/dev/full");
            Pl_StdioFile stage("stdio", file);
            size_t write_count = 0;
            size_t finish_count = 0;
            std::vector<unsigned char> payload(4095, 'x');
            auto const case_status = status([&]() {
                ++write_count;
                stage.write(payload.data(), payload.size());
                ++finish_count;
                stage.finish();
            });
            emit("stdio-4095-enospc", case_status, "", write_count, finish_count);
            fclose(file);
        }

        {
            auto* file = open_buffered("/dev/full");
            Pl_StdioFile stage("stdio", file);
            size_t write_count = 0;
            size_t finish_count = 0;
            std::vector<unsigned char> payload(4096, 'x');
            auto const case_status = status([&]() {
                ++write_count;
                stage.write(payload.data(), payload.size());
                ++finish_count;
                stage.finish();
            });
            emit("stdio-4096-enospc", case_status, "", write_count, finish_count);
            fclose(file);
        }

        {
            auto* file = tmpfile();
            if (file == nullptr) {
                throw std::runtime_error("tmpfile failed");
            }
            if (setvbuf(file, nullptr, _IOFBF, 4096) != 0) {
                fclose(file);
                throw std::runtime_error("setvbuf failed");
            }
            Pl_StdioFile stage("stdio", file);
            size_t write_count = 0;
            size_t finish_count = 0;
            auto const payload = patterned_bytes(4097);
            auto const case_status = status([&]() {
                ++write_count;
                stage.write(payload.data(), payload.size());
                ++finish_count;
                stage.finish();
            });
            auto const bytes = read_regular_file(file);
            emit(
                "stdio-4097-success",
                case_status,
                bytes,
                write_count,
                finish_count);
            fclose(file);
        }

        {
            Cookie cookie;
            cookie.steps.push_back({WriteStepKind::accept, 1024, 0});
            cookie.steps.push_back({WriteStepKind::error, 0, ENOSPC});
            std::array<char, 4096> buffer{};
            auto* file = open_cookie(cookie, buffer);
            Pl_StdioFile stage("stdio", file);
            auto const payload = patterned_bytes(4096);
            auto const case_status = status([&]() {
                during(cookie, CookiePhase::write, [&]() {
                    stage.write(payload.data(), payload.size());
                });
                during(cookie, CookiePhase::finish, [&]() { stage.finish(); });
            });
            close_cookie(file, cookie);
            verify_cookie_lifecycle(
                "stdio-partial-write", cookie, {4096}, {3072}, {});
            emit(
                "stdio-partial-write",
                case_status,
                std::string(cookie.bytes.begin(), cookie.bytes.end()),
                cookie.pipeline_write_count,
                cookie.pipeline_finish_count);
        }

        {
            Cookie cookie;
            // glibc retries EINTR with an internal fopencookie buffer whose
            // contents are not a stable observable. This case records the
            // callback lifecycle only, avoiding a fixture of process memory.
            cookie.capture_bytes = false;
            cookie.steps.push_back({WriteStepKind::interrupted, 0, EINTR});
            cookie.steps.push_back({WriteStepKind::accept, 4096, 0});
            std::array<char, 4096> buffer{};
            auto* file = open_cookie(cookie, buffer);
            Pl_StdioFile stage("stdio", file);
            auto const payload = patterned_bytes(4096);
            auto const case_status = status([&]() {
                during(cookie, CookiePhase::write, [&]() {
                    stage.write(payload.data(), payload.size());
                });
            });
            close_cookie(file, cookie);
            verify_cookie_lifecycle(
                "stdio-interrupted-write", cookie, {4096, 4096}, {}, {1});
            emit(
                "stdio-interrupted-write",
                case_status,
                std::string(cookie.bytes.begin(), cookie.bytes.end()),
                cookie.pipeline_write_count,
                cookie.pipeline_finish_count);
        }

        {
            Cookie cookie;
            cookie.steps.push_back({WriteStepKind::zero, 0, ENOSPC});
            std::array<char, 4096> buffer{};
            auto* file = open_cookie(cookie, buffer);
            Pl_StdioFile stage("stdio", file);
            auto const payload = patterned_bytes(4096);
            auto const raw_status = status([&]() {
                during(cookie, CookiePhase::write, [&]() {
                    stage.write(payload.data(), payload.size());
                });
            });
            if (raw_status !=
                "stdio: Pl_StdioFile::write: No space left on device") {
                throw std::runtime_error(
                    "stdio-zero-progress: unexpected qpdf runtime error");
            }
            close_cookie(file, cookie);
            verify_cookie_lifecycle(
                "stdio-zero-progress", cookie, {4096}, {}, {});
            emit(
                "stdio-zero-progress",
                "runtime",
                std::string(cookie.bytes.begin(), cookie.bytes.end()),
                cookie.pipeline_write_count,
                cookie.pipeline_finish_count);
        }

        {
            Cookie cookie;
            cookie.steps.push_back({WriteStepKind::error, 0, EBADF});
            std::array<char, 4096> buffer{};
            auto* file = open_cookie(cookie, buffer);
            Pl_StdioFile stage("stdio", file);
            auto const case_status = status([&]() {
                during(
                    cookie,
                    CookiePhase::write,
                    [&]() { stage.writeCStr("abc"); });
                during(cookie, CookiePhase::finish, [&]() { stage.finish(); });
            });
            close_cookie(file, cookie);
            verify_cookie_lifecycle(
                "stdio-finish-ebadf", cookie, {}, {3}, {});
            emit(
                "stdio-finish-ebadf",
                case_status,
                std::string(cookie.bytes.begin(), cookie.bytes.end()),
                cookie.pipeline_write_count,
                cookie.pipeline_finish_count);
        }

        {
            Cookie cookie;
            cookie.steps.push_back({WriteStepKind::error, 0, ENOSPC});
            std::array<char, 4096> buffer{};
            auto* file = open_cookie(cookie, buffer);
            Pl_StdioFile stage("stdio", file);
            auto const case_status = status([&]() {
                during(
                    cookie,
                    CookiePhase::write,
                    [&]() { stage.writeCStr("abc"); });
                during(cookie, CookiePhase::finish, [&]() { stage.finish(); });
            });
            close_cookie(file, cookie);
            verify_cookie_lifecycle(
                "stdio-finish-enospc", cookie, {}, {3}, {});
            emit(
                "stdio-finish-enospc",
                case_status,
                std::string(cookie.bytes.begin(), cookie.bytes.end()),
                cookie.pipeline_write_count,
                cookie.pipeline_finish_count);
        }

        {
            Cookie cookie;
            std::array<char, 4096> buffer{};
            auto* file = open_cookie(cookie, buffer);
            Pl_StdioFile stage("stdio", file);
            auto const case_status = status([&]() {
                during(
                    cookie,
                    CookiePhase::write,
                    [&]() { stage.writeCStr("before"); });
                during(cookie, CookiePhase::finish, [&]() { stage.finish(); });
                during(
                    cookie,
                    CookiePhase::write,
                    [&]() { stage.writeCStr("after"); });
                during(cookie, CookiePhase::finish, [&]() { stage.finish(); });
            });
            close_cookie(file, cookie);
            verify_cookie_lifecycle(
                "stdio-repeated-finish", cookie, {}, {6, 5}, {});
            emit(
                "stdio-repeated-finish",
                case_status,
                std::string(cookie.bytes.begin(), cookie.bytes.end()),
                cookie.pipeline_write_count,
                cookie.pipeline_finish_count);
        }
    }
#endif
}

int main(int argc, char* argv[])
{
    if ((argc != 2) ||
        ((std::string(argv[1]) != "core") && (std::string(argv[1]) != "stdio"))) {
        std::cerr << "qpdf_json_pipeline_probe: usage: core|stdio\n";
        return 2;
    }

    try {
        if (std::string(argv[1]) == "core") {
            run_core();
        } else {
#ifdef __linux__
            run_stdio();
#else
            throw std::runtime_error("stdio mode requires Linux");
#endif
        }
        return 0;
    } catch (std::exception const& error) {
        std::cerr << "qpdf_json_pipeline_probe: " << error.what() << '\n';
        return 2;
    }
}
