// Usage: build against pinned qpdf 11.9.0 and run without arguments.
#include <qpdf/Pl_Buffer.hh>
#include <qpdf/QPDF.hh>
#include <qpdf/QPDFObjectHandle.hh>
#include <qpdf/QPDFPageObjectHelper.hh>

#include <iostream>
#include <memory>
#include <stdexcept>

class PassFilter: public QPDFObjectHandle::TokenFilter
{
  public:
    void handleToken(QPDFTokenizer::Token const& token) override
    {
        writeToken(token);
    }
};

class FalseProvider: public QPDFObjectHandle::StreamDataProvider
{
  public:
    FalseProvider(): StreamDataProvider(true)
    {
    }

    bool provideStreamData(int, int, Pipeline*, bool, bool) override
    {
        return false;
    }
};

class ThrowProvider: public QPDFObjectHandle::StreamDataProvider
{
  public:
    ThrowProvider(): StreamDataProvider(true)
    {
    }

    bool provideStreamData(int, int, Pipeline*, bool, bool) override
    {
        throw std::runtime_error("provider failure");
    }
};

class ThrowSink: public Pipeline
{
  public:
    ThrowSink(): Pipeline("throw sink", nullptr)
    {
    }

    void write(unsigned char const*, size_t) override
    {
        throw std::runtime_error("sink failure");
    }

    void finish() override
    {
        throw std::runtime_error("sink finish failure");
    }
};

template <typename F>
void run_case(char const* label, F f)
{
    try {
        f();
        std::cout << label << "=ok\n";
    } catch (std::exception const& e) {
        std::cout << label << "=error " << e.what() << "\n";
    }
}

QPDFObjectHandle
make_form(QPDFObjectHandle stream)
{
    auto dict = stream.getDict();
    dict.replaceKey("/Type", QPDFObjectHandle::newName("/XObject"));
    dict.replaceKey("/Subtype", QPDFObjectHandle::newName("/Form"));
    return stream;
}

int
main()
{
    QPDF pdf;
    pdf.emptyPDF();
    PassFilter filter;
    Pl_Buffer sink("sink");

    auto false_stream = make_form(pdf.newStream());
    false_stream.replaceStreamData(
        std::make_shared<FalseProvider>(), QPDFObjectHandle(), QPDFObjectHandle());
    run_case("form_pipe_false", [&] { QPDFPageObjectHelper(false_stream).pipeContents(&sink); });
    run_case("form_filter_false", [&] {
        QPDFPageObjectHelper(false_stream).filterContents(&filter, &sink);
    });

    auto throw_stream = make_form(pdf.newStream());
    throw_stream.replaceStreamData(
        std::make_shared<ThrowProvider>(), QPDFObjectHandle(), QPDFObjectHandle());
    run_case("form_pipe_throw", [&] { QPDFPageObjectHelper(throw_stream).pipeContents(&sink); });
    run_case("form_filter_throw", [&] {
        QPDFPageObjectHelper(throw_stream).filterContents(&filter, &sink);
    });

    auto setter_reject = make_form(pdf.newStream("q Q\n"));
    setter_reject.getDict().replaceKey("/Filter", QPDFObjectHandle::newName("/FlateDecode"));
    setter_reject.getDict().replaceKey(
        "/DecodeParms", QPDFObjectHandle::parse("<< /Columns (bad) >>"));
    run_case("form_filter_setter_reject", [&] {
        QPDFPageObjectHelper(setter_reject).filterContents(&filter, &sink);
    });

    auto unknown_filter = make_form(pdf.newStream("q Q\n"));
    unknown_filter.getDict().replaceKey(
        "/Filter", QPDFObjectHandle::newName("/UnknownFilter"));
    run_case("form_pipe_unknown_filter", [&] {
        QPDFPageObjectHelper(unknown_filter).pipeContents(&sink);
    });
    run_case("form_filter_unknown_filter", [&] {
        QPDFPageObjectHelper(unknown_filter).filterContents(&filter, &sink);
    });

    auto sink_stream = make_form(pdf.newStream("q Q\n"));
    ThrowSink throwing_sink;
    run_case("form_pipe_sink_throw", [&] {
        QPDFPageObjectHelper(sink_stream).pipeContents(&throwing_sink);
    });
}
