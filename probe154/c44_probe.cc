// C44 probe (flpdf-3yn9.48.154): measure qpdf 11.9.0's deferred
// `StreamBlobProvider` path so the same observations can be pinned on flpdf's
// `ObjectHandle::get_stream_json` (C-U2).
//
// The three questions the route-matrix row leaves open:
//   1. provider invocation count: one probe pipe while `getStreamJSON`
//      resolves the effective decode level, then one more pipe per blob
//      serialization (`libqpdf/QPDF_Stream.cc:96-107,186-204`);
//   2. filter / payload: the inline JSON dictionary drops `/Filter` exactly
//      when the blob payload was decoded, and the blob bytes are the same
//      bytes the `writeStreamJSON` probe saw;
//   3. blob lifetime: the blob holds the live `QPDF_Stream` and reads it at
//      serialization time, not at `getStreamJSON` time.
//
// It also measures the `nullptr` `filtering_attempted` argument of
// `StreamBlobProvider::operator()` (`libqpdf/QPDF_Stream.cc:106`) against a
// real `bool*`: `pipeStreamData` substitutes a local `ignored`
// (`libqpdf/QPDF_Stream.cc:499-501`), so both forms must pipe identical bytes
// and invoke the provider the same number of times.
//
// Build/run: probe154/c44_probe.sh

#define POINTERHOLDER_TRANSITION 4

#include <qpdf/Buffer.hh>
#include <qpdf/Pl_Buffer.hh>
#include <qpdf/Pl_Flate.hh>
#include <qpdf/QPDF.hh>
#include <qpdf/QPDFObjectHandle.hh>

#include <iostream>
#include <memory>
#include <string>

class CountingProvider: public QPDFObjectHandle::StreamDataProvider
{
  public:
    explicit CountingProvider(std::string data) :
        data(std::move(data))
    {
    }

    void
    provideStreamData(QPDFObjGen const&, Pipeline* p) override
    {
        ++calls;
        p->writeString(data);
        p->finish();
    }

    int calls{0};
    std::string data;
};

class ThrowingProvider: public QPDFObjectHandle::StreamDataProvider
{
  public:
    void
    provideStreamData(QPDFObjGen const&, Pipeline* p) override
    {
        ++calls;
        if (calls == 2) {
            throw std::logic_error("blob provider logic failure");
        }
        p->writeString("throw");
        p->finish();
    }

    int calls{0};
};

static std::shared_ptr<QPDFObjectHandle::StreamDataProvider>
counting_provider(std::shared_ptr<CountingProvider> const& provider)
{
    return std::static_pointer_cast<QPDFObjectHandle::StreamDataProvider>(provider);
}

static std::string
deflate_string(std::string const& value)
{
    Pl_Buffer compressed{"compressed"};
    Pl_Flate flate{"deflate", &compressed, Pl_Flate::a_deflate};
    flate.writeString(value);
    flate.finish();
    return compressed.getString();
}

static QPDFObjectHandle
new_provider_stream(QPDF& pdf, std::shared_ptr<CountingProvider> const& provider)
{
    QPDFObjectHandle stream = QPDFObjectHandle::newStream(&pdf);
    stream.replaceStreamData(
        counting_provider(provider), QPDFObjectHandle::newNull(), QPDFObjectHandle::newNull());
    return stream;
}

int
main()
{
    // A: provider call count across getStreamJSON + two serializations.
    {
        QPDF pdf;
        pdf.emptyPDF();
        auto provider = std::make_shared<CountingProvider>("hello provider");
        QPDFObjectHandle stream = new_provider_stream(pdf, provider);
        JSON json = stream.getStreamJSON(2, qpdf_sj_inline, qpdf_dl_none, nullptr, "");
        std::cout << "A.calls_after_get=" << provider->calls << "\n";
        std::string first = json.unparse();
        std::cout << "A.calls_after_unparse=" << provider->calls << "\n";
        std::cout << "A.json=" << first << "\n";
        std::string second = json.unparse();
        std::cout << "A.calls_after_second_unparse=" << provider->calls << "\n";
        std::cout << "A.second_unparse_identical=" << (first == second ? "yes" : "no") << "\n";
    }

    // B: nullptr vs real filtering_attempted in pipeStreamData.
    {
        QPDF pdf;
        pdf.emptyPDF();
        auto provider_null = std::make_shared<CountingProvider>("abcdef");
        QPDFObjectHandle stream_null = new_provider_stream(pdf, provider_null);
        Pl_Buffer buffer_null{"null-filterp"};
        bool ok_null = stream_null.pipeStreamData(
            &buffer_null, nullptr, 0, qpdf_dl_none, false, false);
        std::string bytes_null = buffer_null.getString();

        QPDF pdf2;
        pdf2.emptyPDF();
        auto provider_real = std::make_shared<CountingProvider>("abcdef");
        QPDFObjectHandle stream_real = new_provider_stream(pdf2, provider_real);
        Pl_Buffer buffer_real{"real-filterp"};
        bool filtering_attempted = false;
        bool ok_real = stream_real.pipeStreamData(
            &buffer_real, &filtering_attempted, 0, qpdf_dl_none, false, false);
        std::string bytes_real = buffer_real.getString();

        std::cout << "B.null_filterp_calls=" << provider_null->calls
                  << " bytes=" << bytes_null << " ok=" << (ok_null ? "true" : "false") << "\n";
        std::cout << "B.real_filterp_calls=" << provider_real->calls
                  << " bytes=" << bytes_real << " ok=" << (ok_real ? "true" : "false")
                  << " filtering_attempted=" << (filtering_attempted ? "true" : "false") << "\n";
        std::cout << "B.identical=" << (bytes_null == bytes_real ? "yes" : "no") << "\n";
    }

    // C: buffer-backed /FlateDecode stream at two decode levels. The inline
    // JSON dictionary must drop /Filter only for the derived-level blob.
    {
        std::string raw = "hello flate payload";
        std::string deflated = deflate_string(raw);
        QPDF pdf;
        pdf.emptyPDF();
        QPDFObjectHandle stream = QPDFObjectHandle::newStream(&pdf);
        stream.replaceStreamData(
            deflated,
            QPDFObjectHandle::newName("/FlateDecode"),
            QPDFObjectHandle::newNull());
        JSON decoded = stream.getStreamJSON(2, qpdf_sj_inline, qpdf_dl_generalized, nullptr, "");
        std::cout << "C.generalized=" << decoded.unparse() << "\n";
        JSON raw_json = stream.getStreamJSON(2, qpdf_sj_inline, qpdf_dl_none, nullptr, "");
        std::cout << "C.none=" << raw_json.unparse() << "\n";
    }

    // D: the blob reads the live stream at serialization time.
    {
        QPDF pdf;
        pdf.emptyPDF();
        auto provider = std::make_shared<CountingProvider>("AAAA");
        QPDFObjectHandle stream = new_provider_stream(pdf, provider);
        JSON json = stream.getStreamJSON(2, qpdf_sj_inline, qpdf_dl_none, nullptr, "");
        provider->data = "BBBB";
        std::cout << "D.after_mutation=" << json.unparse() << "\n";
    }

    // E: a provider logic_error raised while the blob serializes propagates.
    {
        QPDF pdf;
        pdf.emptyPDF();
        QPDFObjectHandle stream = QPDFObjectHandle::newStream(&pdf);
        auto provider = std::make_shared<ThrowingProvider>();
        stream.replaceStreamData(
            std::static_pointer_cast<QPDFObjectHandle::StreamDataProvider>(provider),
            QPDFObjectHandle::newNull(),
            QPDFObjectHandle::newNull());
        JSON json = stream.getStreamJSON(2, qpdf_sj_inline, qpdf_dl_none, nullptr, "");
        std::cout << "E.calls_after_get=" << provider->calls << "\n";
        try {
            json.unparse();
            std::cout << "E.unparse=no_exception\n";
        } catch (std::logic_error const& error) {
            std::cout << "E.unparse=logic_error:" << error.what() << "\n";
        }
    }

    return 0;
}
