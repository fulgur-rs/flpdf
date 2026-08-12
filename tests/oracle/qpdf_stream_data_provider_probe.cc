#include <qpdf/Buffer.hh>
#include <qpdf/Pl_Buffer.hh>
#include <qpdf/QPDF.hh>
#include <qpdf/QPDFObjGen.hh>
#include <qpdf/QPDFObjectHandle.hh>

#include <iostream>
#include <memory>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

namespace
{
void
require(bool condition, std::string const& message)
{
    if (!condition) {
        throw std::runtime_error(message);
    }
}

std::string
buffer_string(std::shared_ptr<Buffer> const& buffer)
{
    if (buffer->getSize() == 0) {
        return {};
    }
    return std::string(
        reinterpret_cast<char const*>(buffer->getBuffer()),
        buffer->getSize());
}

class LegacyProvider: public QPDFObjectHandle::StreamDataProvider
{
  public:
    explicit LegacyProvider(std::string payload) :
        StreamDataProvider(false),
        payload(std::move(payload))
    {
    }

    void
    provideStreamData(int objid, int generation, Pipeline* pipeline) override
    {
        ++calls;
        object_generations.emplace_back(objid, generation);
        if (!payload.empty()) {
            pipeline->write(
                reinterpret_cast<unsigned char const*>(payload.data()),
                payload.size());
        }
        pipeline->finish();
    }

    std::string payload;
    int calls{0};
    std::vector<QPDFObjGen> object_generations;
};

class RetryProvider: public QPDFObjectHandle::StreamDataProvider
{
  public:
    RetryProvider(std::string payload, bool result) :
        StreamDataProvider(true),
        payload(std::move(payload)),
        result(result)
    {
    }

    bool
    provideStreamData(
        int objid,
        int generation,
        Pipeline* pipeline,
        bool suppress_warnings,
        bool will_retry) override
    {
        ++calls;
        object_generations.emplace_back(objid, generation);
        saw_suppress_warnings = suppress_warnings;
        saw_will_retry = will_retry;
        if (!payload.empty()) {
            pipeline->write(
                reinterpret_cast<unsigned char const*>(payload.data()),
                payload.size());
        }
        pipeline->finish();
        return result;
    }

    std::string payload;
    bool result;
    int calls{0};
    bool saw_suppress_warnings{false};
    bool saw_will_retry{false};
    std::vector<QPDFObjGen> object_generations;
};

void
check_identity(
    std::vector<QPDFObjGen> const& object_generations,
    QPDFObjectHandle const& stream,
    size_t expected_calls,
    std::string const& context)
{
    require(object_generations.size() == expected_calls, context + " provider call count differs");
    for (auto const& object_generation: object_generations) {
        require(
            object_generation == stream.getObjGen(),
            context + " provider object identity differs");
    }
}
}

int
main()
{
    try {
        QPDF pdf;
        pdf.emptyPDF();

        // Provider registration is lazy, repeated calls produce the same bytes, and qpdf retains
        // the provider allocation after the caller releases its shared_ptr.
        auto lazy_stream = pdf.newStream();
        auto lazy_provider = std::make_shared<LegacyProvider>("provider");
        std::weak_ptr<LegacyProvider> lazy_ownership = lazy_provider;
        lazy_stream.replaceStreamData(
            lazy_provider,
            QPDFObjectHandle::newNull(),
            QPDFObjectHandle::newNull());
        require(lazy_provider->calls == 0, "provider was called during registration");
        require(!lazy_provider->supportsRetry(), "legacy provider unexpectedly supports retry");
        lazy_provider.reset();
        require(!lazy_ownership.expired(), "stream did not retain provider ownership");

        auto lazy_first = lazy_stream.getRawStreamData();
        auto retained_lazy_provider = lazy_ownership.lock();
        require(retained_lazy_provider != nullptr, "retained provider disappeared");
        require(buffer_string(lazy_first) == "provider", "first provider payload differs");
        require(
            lazy_stream.getDict().getKey("/Length").getIntValue() == 8,
            "provider length was not installed");
        require(retained_lazy_provider->calls == 1, "first provider call count differs");
        check_identity(
            retained_lazy_provider->object_generations,
            lazy_stream,
            1,
            "first");

        auto lazy_second = lazy_stream.getRawStreamData();
        require(buffer_string(lazy_second) == "provider", "repeated provider payload differs");
        require(retained_lazy_provider->calls == 2, "repeated provider call count differs");
        check_identity(
            retained_lazy_provider->object_generations,
            lazy_stream,
            2,
            "repeated");

        // An uninitialized filter/decode handle preserves existing dictionary entries. A null
        // handle removes them.
        auto filter_stream = pdf.newStream();
        filter_stream.getDict().replaceKey(
            "/Filter",
            QPDFObjectHandle::newName("/KeepFilter"));
        filter_stream.getDict().replaceKey(
            "/DecodeParms",
            QPDFObjectHandle::newDictionary());
        auto preserve_provider = std::make_shared<LegacyProvider>("preserve");
        filter_stream.replaceStreamData(
            preserve_provider,
            QPDFObjectHandle(),
            QPDFObjectHandle());
        require(
            filter_stream.getDict().hasKey("/Filter"),
            "uninitialized filter did not preserve /Filter");
        require(
            filter_stream.getDict().hasKey("/DecodeParms"),
            "uninitialized decode parms did not preserve /DecodeParms");

        auto remove_provider = std::make_shared<LegacyProvider>("remove");
        filter_stream.replaceStreamData(
            remove_provider,
            QPDFObjectHandle::newNull(),
            QPDFObjectHandle::newNull());
        require(
            !filter_stream.getDict().hasKey("/Filter"),
            "null filter did not remove /Filter");
        require(
            !filter_stream.getDict().hasKey("/DecodeParms"),
            "null decode parms did not remove /DecodeParms");

        // Replacing a provider with a buffer disables the provider, and replacing the buffer with
        // another provider disables the buffer.
        auto replacement_stream = pdf.newStream();
        auto first_provider = std::make_shared<LegacyProvider>("first");
        replacement_stream.replaceStreamData(
            first_provider,
            QPDFObjectHandle::newNull(),
            QPDFObjectHandle::newNull());
        require(
            buffer_string(replacement_stream.getRawStreamData()) == "first",
            "initial provider payload differs");
        require(first_provider->calls == 1, "initial provider was not called once");

        auto replacement_buffer = std::make_shared<Buffer>(std::string("buffer"));
        replacement_stream.replaceStreamData(
            replacement_buffer,
            QPDFObjectHandle::newNull(),
            QPDFObjectHandle::newNull());
        require(
            buffer_string(replacement_stream.getRawStreamData()) == "buffer",
            "buffer replacement payload differs");
        require(first_provider->calls == 1, "replaced provider was called again");

        auto second_provider = std::make_shared<LegacyProvider>("second");
        replacement_stream.replaceStreamData(
            second_provider,
            QPDFObjectHandle::newNull(),
            QPDFObjectHandle::newNull());
        require(
            buffer_string(replacement_stream.getRawStreamData()) == "second",
            "second provider replacement payload differs");
        require(second_provider->calls == 1, "second provider was not called once");

        // A successful empty provider installs an explicit zero /Length.
        auto empty_stream = pdf.newStream();
        auto empty_provider = std::make_shared<LegacyProvider>("");
        empty_stream.replaceStreamData(
            empty_provider,
            QPDFObjectHandle::newNull(),
            QPDFObjectHandle::newNull());
        require(empty_provider->calls == 0, "empty provider was called during registration");
        require(
            buffer_string(empty_stream.getRawStreamData()).empty(),
            "empty provider produced non-empty data");
        require(
            empty_stream.getDict().getKey("/Length").getIntValue() == 0,
            "empty provider did not install zero /Length");

        // A pre-existing /Length is a contract check for provider output.
        auto mismatch_stream = pdf.newStream();
        auto mismatch_provider = std::make_shared<LegacyProvider>("abc");
        mismatch_stream.replaceStreamData(
            mismatch_provider,
            QPDFObjectHandle::newNull(),
            QPDFObjectHandle::newNull());
        mismatch_stream.getDict().replaceKey(
            "/Length",
            QPDFObjectHandle::newInteger(4));
        try {
            mismatch_stream.getRawStreamData();
            throw std::runtime_error("provider length mismatch was not rejected");
        } catch (std::runtime_error const& error) {
            require(
                std::string(error.what()) ==
                    "stream data provider for " + mismatch_stream.getObjGen().unparse(' ') +
                        " provided 3 bytes instead of expected 4 bytes",
                "provider length mismatch error differs");
        }

        // Retry-aware providers receive both flags and their return value controls overall
        // success. Their output is still written to the supplied pipeline.
        auto retry_stream = pdf.newStream();
        auto retry_provider = std::make_shared<RetryProvider>("retry", true);
        require(retry_provider->supportsRetry(), "retry provider does not support retry");
        retry_stream.replaceStreamData(
            retry_provider,
            QPDFObjectHandle::newNull(),
            QPDFObjectHandle::newNull());
        Pl_Buffer retry_output("retry output");
        bool retry_filtering = true;
        require(
            retry_stream.pipeStreamData(
                &retry_output,
                &retry_filtering,
                0,
                qpdf_dl_none,
                true,
                true),
            "successful retry provider reported failure");
        require(retry_output.getString() == "retry", "retry provider payload differs");
        require(!retry_filtering, "retry provider unexpectedly attempted filtering");
        require(retry_provider->calls == 1, "retry provider call count differs");
        require(retry_provider->saw_suppress_warnings, "suppress_warnings was not forwarded");
        require(retry_provider->saw_will_retry, "will_retry was not forwarded");
        check_identity(retry_provider->object_generations, retry_stream, 1, "retry");
        require(
            retry_stream.getDict().getKey("/Length").getIntValue() == 5,
            "retry provider length was not installed");

        auto failed_retry_stream = pdf.newStream();
        auto failed_retry_provider = std::make_shared<RetryProvider>("failed", false);
        failed_retry_stream.replaceStreamData(
            failed_retry_provider,
            QPDFObjectHandle::newNull(),
            QPDFObjectHandle::newNull());
        Pl_Buffer failed_retry_output("failed retry output");
        bool failed_retry_filtering = true;
        require(
            !failed_retry_stream.pipeStreamData(
                &failed_retry_output,
                &failed_retry_filtering,
                0,
                qpdf_dl_none,
                false,
                true),
            "failed retry provider unexpectedly reported success");
        require(
            failed_retry_output.getString() == "failed",
            "failed retry provider payload differs");
        require(!failed_retry_filtering, "failed retry provider filtering flag differs");
        require(
            !failed_retry_stream.getDict().hasKey("/Length"),
            "failed retry provider installed /Length");

        // The base provider is intentionally abstract-by-contract: its default implementation
        // throws the qpdf diagnostic when no callback form is overridden.
        auto default_stream = pdf.newStream();
        auto default_provider = std::make_shared<QPDFObjectHandle::StreamDataProvider>();
        default_stream.replaceStreamData(
            default_provider,
            QPDFObjectHandle::newNull(),
            QPDFObjectHandle::newNull());
        try {
            default_stream.getRawStreamData();
            throw std::runtime_error("default provider unexpectedly produced data");
        } catch (std::logic_error const& error) {
            require(
                std::string(error.what()) ==
                    "you must override provideStreamData -- see QPDFObjectHandle.hh",
                "default provider error differs: " + std::string(error.what()));
        }

        std::cout << "qpdf stream data provider probe: ok\n";
        return 0;
    } catch (std::exception const& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
