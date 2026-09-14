#include <qpdf/Buffer.hh>
#include <qpdf/Pipeline.hh>
#include <qpdf/QPDF.hh>
#include <qpdf/QPDFObjectHandle.hh>
#include <qpdf/QPDFStreamFilter.hh>

#include <iostream>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

namespace
{
struct State
{
    int factory_calls{0};
    int filter_drops{0};
    int pipeline_drops{0};
    int marker{-1};
    std::vector<std::string> construction_order;
};

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
    return std::string(
        reinterpret_cast<char const*>(buffer->getBuffer()),
        buffer->getSize());
}

std::string
join(std::vector<std::string> const& values)
{
    std::string result;
    for (size_t i = 0; i < values.size(); ++i) {
        if (i != 0) {
            result += ',';
        }
        result += values.at(i);
    }
    return result;
}

class PrefixPipeline: public Pipeline
{
  public:
    PrefixPipeline(Pipeline* next, std::string prefix, std::shared_ptr<State> state) :
        Pipeline("qpdf registry prefix", next),
        prefix(std::move(prefix)),
        state(std::move(state))
    {
    }

    ~PrefixPipeline() override
    {
        ++state->pipeline_drops;
    }

    void
    write(unsigned char const* data, size_t len) override
    {
        getNext()->write(
            reinterpret_cast<unsigned char const*>(prefix.data()),
            prefix.size());
        getNext()->write(data, len);
    }

    void
    finish() override
    {
        getNext()->finish();
    }

  private:
    std::string prefix;
    std::shared_ptr<State> state;
};

class PrefixFilter: public QPDFStreamFilter
{
  public:
    PrefixFilter(std::string prefix, std::shared_ptr<State> state) :
        prefix(std::move(prefix)),
        state(std::move(state))
    {
    }

    ~PrefixFilter() override
    {
        ++state->filter_drops;
    }

    bool
    setDecodeParms(QPDFObjectHandle) override
    {
        return true;
    }

    Pipeline*
    getDecodePipeline(Pipeline* next) override
    {
        pipeline = std::make_unique<PrefixPipeline>(next, prefix, state);
        return pipeline.get();
    }

  private:
    std::string prefix;
    std::shared_ptr<State> state;
    std::unique_ptr<Pipeline> pipeline;
};

class ParamsFilter: public QPDFStreamFilter
{
  public:
    explicit ParamsFilter(std::shared_ptr<State> state) : state(std::move(state))
    {
    }

    bool
    setDecodeParms(QPDFObjectHandle decode_parms) override
    {
        state->marker = decode_parms.getKey("/Marker").getIntValue();
        return true;
    }

    Pipeline*
    getDecodePipeline(Pipeline*) override
    {
        return nullptr;
    }

  private:
    std::shared_ptr<State> state;
};

class FailingFilter: public QPDFStreamFilter
{
  public:
    Pipeline*
    getDecodePipeline(Pipeline*) override
    {
        throw std::runtime_error("filter callback failure");
    }
};

QPDFObjectHandle
filter_name(char const* name)
{
    return QPDFObjectHandle::newName(name);
}

QPDFObjectHandle
filter_array(std::initializer_list<char const*> names)
{
    auto result = QPDFObjectHandle::newArray();
    for (auto name: names) {
        result.appendItem(filter_name(name));
    }
    return result;
}

std::string
decode_alias_stream()
{
    // zlib-wrapped bytes for the ASCII payload "alias".
    std::string compressed{
        static_cast<char>(0x78), static_cast<char>(0x9c), static_cast<char>(0x4b),
        static_cast<char>(0xcc), static_cast<char>(0xc9), static_cast<char>(0x4c),
        static_cast<char>(0x2c), static_cast<char>(0x06), static_cast<char>(0x00),
        static_cast<char>(0x06), static_cast<char>(0x0a), static_cast<char>(0x02),
        static_cast<char>(0x0b)};
    QPDF pdf;
    pdf.emptyPDF();
    auto stream = pdf.newStream(compressed);
    stream.getDict().replaceKey("/Filter", filter_name("/Fl"));
    return buffer_string(stream.getStreamData(qpdf_dl_generalized));
}
}

int
main()
{
    try {
        auto replacement_state = std::make_shared<State>();
        QPDF::registerStreamFilter(
            "/FlpdfRegistryPrefix",
            [replacement_state]() {
                ++replacement_state->factory_calls;
                return std::make_shared<PrefixFilter>("first:", replacement_state);
            });

        QPDF replacement_pdf;
        replacement_pdf.emptyPDF();
        auto first_stream = replacement_pdf.newStream("payload");
        first_stream.getDict().replaceKey("/Filter", filter_name("/FlpdfRegistryPrefix"));
        auto first = buffer_string(first_stream.getStreamData(qpdf_dl_generalized));
        require(first == "first:payload", "initial registered filter output differs");

        QPDF::registerStreamFilter(
            "/FlpdfRegistryPrefix",
            [replacement_state]() {
                ++replacement_state->factory_calls;
                return std::make_shared<PrefixFilter>("second:", replacement_state);
            });
        auto second_stream = replacement_pdf.newStream("payload");
        second_stream.getDict().replaceKey("/Filter", filter_name("/FlpdfRegistryPrefix"));
        auto second = buffer_string(second_stream.getStreamData(qpdf_dl_generalized));
        require(second == "second:payload", "replacement registered filter output differs");
        require(replacement_state->factory_calls == 2, "replacement factory count differs");
        std::cout << "replacement.first=" << first << '\n';
        std::cout << "replacement.second=" << second << '\n';
        std::cout << "replacement.factory_calls=" << replacement_state->factory_calls << '\n';

        auto params_state = std::make_shared<State>();
        QPDF::registerStreamFilter(
            "/FlpdfRegistryParams",
            [params_state]() {
                ++params_state->factory_calls;
                return std::make_shared<ParamsFilter>(params_state);
            });
        QPDF params_pdf;
        params_pdf.emptyPDF();
        auto params_stream = params_pdf.newStream("payload");
        params_stream.getDict().replaceKey("/Filter", filter_name("/FlpdfRegistryParams"));
        auto params = QPDFObjectHandle::newDictionary();
        params.replaceKey("/Marker", QPDFObjectHandle::newInteger(7));
        params_stream.getDict().replaceKey("/DecodeParms", params);
        auto params_output = buffer_string(params_stream.getStreamData(qpdf_dl_generalized));
        require(params_output == "payload", "decode params filter output differs");
        require(params_state->marker == 7, "full decode params handle was not delivered");
        std::cout << "decode_params.marker=" << params_state->marker << '\n';

        QPDF callback_pdf;
        callback_pdf.emptyPDF();
        QPDF::registerStreamFilter(
            "/FlpdfRegistryFilterError",
            []() { return std::make_shared<FailingFilter>(); });
        auto callback_stream = callback_pdf.newStream("payload");
        callback_stream.getDict().replaceKey(
            "/Filter", filter_name("/FlpdfRegistryFilterError"));
        try {
            callback_stream.getStreamData(qpdf_dl_generalized);
            throw std::runtime_error("filter callback unexpectedly succeeded");
        } catch (std::runtime_error const& error) {
            require(
                error.what() == std::string("filter callback failure"),
                "filter callback error differs");
        }
        std::cout << "callback.error=filter callback failure\n";

        auto order_state = std::make_shared<State>();
        QPDF::registerStreamFilter(
            "/FlpdfRegistryKnownBeforeUnknown",
            [order_state]() {
                order_state->construction_order.push_back("first");
                return std::make_shared<ParamsFilter>(order_state);
            });
        QPDF::registerStreamFilter(
            "/FlpdfRegistryKnownAfterUnknown",
            [order_state]() {
                order_state->construction_order.push_back("last");
                return std::make_shared<ParamsFilter>(order_state);
            });
        QPDF order_pdf;
        order_pdf.emptyPDF();
        auto order_stream = order_pdf.newStream("payload");
        order_stream.getDict().replaceKey(
            "/Filter",
            filter_array({
                "/FlpdfRegistryKnownBeforeUnknown",
                "/FlpdfRegistryUnknown",
                "/FlpdfRegistryKnownAfterUnknown"}));
        bool filtering_attempted = true;
        auto order_result = order_stream.pipeStreamData(
            nullptr,
            &filtering_attempted,
            0,
            qpdf_dl_generalized);
        require(!order_result, "unknown filter unexpectedly remained filterable");
        require(!filtering_attempted, "unknown filter was reported as attempted");
        require(
            order_state->construction_order == std::vector<std::string>{"first", "last"},
            "unknown filter construction order differs");
        std::cout << "unknown.filterable=false\n";
        std::cout << "unknown.factory_order=" << join(order_state->construction_order) << '\n';

        auto slash_state = std::make_shared<State>();
        QPDF::registerStreamFilter(
            "/FlpdfRegistrySlashKey",
            [slash_state]() {
                ++slash_state->factory_calls;
                return std::make_shared<PrefixFilter>("ordinary:", slash_state);
            });
        QPDF::registerStreamFilter(
            "//FlpdfRegistrySlashKey",
            [slash_state]() {
                ++slash_state->factory_calls;
                return std::make_shared<PrefixFilter>("escaped:", slash_state);
            });
        QPDF slash_pdf;
        slash_pdf.emptyPDF();
        auto ordinary_slash_stream = slash_pdf.newStream("payload");
        ordinary_slash_stream.getDict().replaceKey(
            "/Filter", filter_name("/FlpdfRegistrySlashKey"));
        auto ordinary_slash =
            buffer_string(ordinary_slash_stream.getStreamData(qpdf_dl_generalized));
        auto escaped_slash_stream = slash_pdf.newStream("payload");
        escaped_slash_stream.getDict().replaceKey(
            "/Filter", QPDFObjectHandle::parse("/#2FFlpdfRegistrySlashKey"));
        auto escaped_slash =
            buffer_string(escaped_slash_stream.getStreamData(qpdf_dl_generalized));
        require(ordinary_slash == "ordinary:payload", "ordinary slash key output differs");
        require(escaped_slash == "escaped:payload", "escaped slash key output differs");
        require(slash_state->factory_calls == 2, "slash key factory count differs");
        std::cout << "escaped_slash.ordinary=" << ordinary_slash << '\n';
        std::cout << "escaped_slash.escaped=" << escaped_slash << '\n';

        auto alias_state = std::make_shared<State>();
        QPDF::registerStreamFilter(
            "/Fl",
            [alias_state]() {
                ++alias_state->factory_calls;
                return std::make_shared<PrefixFilter>("wrong:", alias_state);
            });
        auto alias_output = decode_alias_stream();
        require(alias_output == "alias", "filter abbreviation did not reach FlateDecode");
        require(alias_state->factory_calls == 0, "filter abbreviation used /Fl override");
        std::cout << "alias.output=" << alias_output << '\n';
        std::cout << "alias.override_factory_calls=" << alias_state->factory_calls << '\n';

        auto lifetime_state = std::make_shared<State>();
        QPDF::registerStreamFilter(
            "/FlpdfRegistryLifetime",
            [lifetime_state]() {
                ++lifetime_state->factory_calls;
                return std::make_shared<PrefixFilter>("lifetime:", lifetime_state);
            });
        {
            QPDF lifetime_pdf;
            lifetime_pdf.emptyPDF();
            auto lifetime_stream = lifetime_pdf.newStream("payload");
            lifetime_stream.getDict().replaceKey(
                "/Filter", filter_name("/FlpdfRegistryLifetime"));
            auto lifetime_output = buffer_string(lifetime_stream.getStreamData(qpdf_dl_generalized));
            require(lifetime_output == "lifetime:payload", "lifetime filter output differs");
        }
        require(lifetime_state->factory_calls == 1, "lifetime factory count differs");
        require(lifetime_state->filter_drops == 1, "filter destruction count differs");
        require(lifetime_state->pipeline_drops == 1, "pipeline destruction count differs");
        std::cout << "lifetime.filter_drops=" << lifetime_state->filter_drops << '\n';
        std::cout << "lifetime.pipeline_drops=" << lifetime_state->pipeline_drops << '\n';

        std::cout << "qpdf_stream_filter_registry_probe=ok\n";
        return 0;
    } catch (std::exception const& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
