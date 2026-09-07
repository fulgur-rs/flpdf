// Usage: probe OUTPUT_PREFIX; build against pinned qpdf 11.9.0.
#include <qpdf/QPDF.hh>
#include <qpdf/QPDFWriter.hh>
#include <iostream>
#include <memory>
#include <string>
#include <vector>

namespace
{
void
add_external_keys(QPDFObjectHandle stream)
{
    auto dict = stream.getDict();
    dict.replaceKey("/F", QPDFObjectHandle::newString("external.bin"));
    dict.replaceKey("/FFilter", QPDFObjectHandle::newName("/ASCIIHexDecode"));
    dict.replaceKey("/FDecodeParms", QPDFObjectHandle::parse("<< /Marker 42 >>"));
}

std::string
write_and_read(QPDF& pdf, std::string const& path, bool compress)
{
    QPDFWriter writer(pdf, path.c_str());
    writer.setCompressStreams(compress);
    writer.setStaticID(true);
    writer.write();
    QPDF result;
    result.processFile(path.c_str());
    return result.getTrailer().getKey("/Extra").getDict().unparse();
}

class RetryProvider: public QPDFObjectHandle::StreamDataProvider
{
  public:
    explicit RetryProvider(int& calls, std::vector<bool>& retries):
        StreamDataProvider(true),
        calls(calls),
        retries(retries)
    {
    }

    bool provideStreamData(
        int,
        int,
        Pipeline* pipeline,
        bool,
        bool will_retry) override
    {
        ++calls;
        retries.push_back(will_retry);
        pipeline->write("provider", 8);
        pipeline->finish();
        return !will_retry;
    }

  private:
    int& calls;
    std::vector<bool>& retries;
};

class PassFilter: public QPDFObjectHandle::TokenFilter
{
  public:
    explicit PassFilter(int& eof_calls):
        eof_calls(eof_calls)
    {
    }

    void handleToken(QPDFTokenizer::Token const& token) override
    {
        writeToken(token);
    }

    void handleEOF() override
    {
        ++eof_calls;
    }

  private:
    int& eof_calls;
};
}

int main(int argc, char** argv)
{
    if (argc != 2) {
        return 2;
    }
    std::string prefix = argv[1];

    QPDF refiltered;
    refiltered.emptyPDF();
    auto refiltered_stream = refiltered.newStream("q Q\n");
    add_external_keys(refiltered_stream);
    refiltered.getTrailer().replaceKey("/Extra", refiltered_stream);
    std::cout << "refiltered="
              << write_and_read(refiltered, prefix + ".refiltered.pdf", true) << '\n';

    QPDF decoded;
    decoded.emptyPDF();
    auto decoded_stream = decoded.newStream("71>\n");
    add_external_keys(decoded_stream);
    decoded_stream.getDict().replaceKey("/Filter", QPDFObjectHandle::newName("/ASCIIHexDecode"));
    decoded_stream.setFilterOnWrite(true);
    decoded.getTrailer().replaceKey("/Extra", decoded_stream);
    QPDFWriter decoded_writer(decoded, (prefix + ".decoded.pdf").c_str());
    decoded_writer.setCompressStreams(false);
    decoded_writer.setDecodeLevel(qpdf_dl_generalized);
    decoded_writer.setStaticID(true);
    decoded_writer.write();
    QPDF decoded_result;
    decoded_result.processFile((prefix + ".decoded.pdf").c_str());
    std::cout << "decoded=" << decoded_result.getTrailer().getKey("/Extra").getDict().unparse()
              << '\n';

    QPDF veto;
    veto.emptyPDF();
    auto veto_stream = veto.newStream("71>\n");
    add_external_keys(veto_stream);
    veto_stream.getDict().replaceKey("/Filter", QPDFObjectHandle::newName("/ASCIIHexDecode"));
    veto_stream.setFilterOnWrite(false);
    veto.getTrailer().replaceKey("/Extra", veto_stream);
    std::cout << "veto=" << write_and_read(veto, prefix + ".veto.pdf", true) << '\n';

    QPDF metadata;
    metadata.emptyPDF();
    auto metadata_stream = metadata.newStream("71>\n");
    add_external_keys(metadata_stream);
    metadata_stream.getDict().replaceKey("/Type", QPDFObjectHandle::newName("/Metadata"));
    metadata_stream.getDict().replaceKey("/Filter", QPDFObjectHandle::newName("/ASCIIHexDecode"));
    metadata.getTrailer().replaceKey("/Extra", metadata_stream);
    std::cout << "metadata=" << write_and_read(metadata, prefix + ".metadata.pdf", true) << '\n';

    int provider_calls = 0;
    std::vector<bool> retries;
    QPDF provider_pdf;
    provider_pdf.emptyPDF();
    auto provider_stream = provider_pdf.newStream();
    add_external_keys(provider_stream);
    provider_stream.replaceStreamData(
        std::make_shared<RetryProvider>(provider_calls, retries),
        QPDFObjectHandle::newNull(),
        QPDFObjectHandle::newNull());
    provider_pdf.getTrailer().replaceKey("/Extra", provider_stream);
    std::cout << "provider=" << write_and_read(provider_pdf, prefix + ".provider.pdf", true)
              << " calls=" << provider_calls << " retry=";
    for (bool retry: retries) {
        std::cout << (retry ? '1' : '0');
    }
    std::cout << '\n';

    int eof_calls = 0;
    QPDF token_pdf;
    token_pdf.emptyPDF();
    auto token_stream = token_pdf.newStream("q Q\n");
    add_external_keys(token_stream);
    token_stream.addTokenFilter(std::make_shared<PassFilter>(eof_calls));
    token_pdf.getTrailer().replaceKey("/Extra", token_stream);
    std::cout << "token=" << write_and_read(token_pdf, prefix + ".token.pdf", true)
              << " eof=" << eof_calls << '\n';
}
