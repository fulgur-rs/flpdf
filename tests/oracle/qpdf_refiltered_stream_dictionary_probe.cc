// Usage: probe OUTPUT.pdf; build against pinned qpdf 11.9.0.
#include <qpdf/QPDF.hh>
#include <qpdf/QPDFWriter.hh>
#include <iostream>
int main(int argc, char** argv)
{
    if (argc != 2) {
        return 2;
    }
    QPDF pdf;
    pdf.emptyPDF();
    auto stream = pdf.newStream("q Q\n");
    auto dict = stream.getDict();
    dict.replaceKey("/F", QPDFObjectHandle::newString("external.bin"));
    dict.replaceKey("/FFilter", QPDFObjectHandle::newName("/ASCIIHexDecode"));
    dict.replaceKey("/FDecodeParms", QPDFObjectHandle::parse("<< /Marker 42 >>"));
    pdf.getTrailer().replaceKey("/Extra", stream);
    QPDFWriter writer(pdf, argv[1]);
    writer.setCompressStreams(true);
    writer.setStaticID(true);
    writer.write();
    QPDF result;
    result.processFile(argv[1]);
    std::cout << result.getTrailer().getKey("/Extra").getDict().unparse() << '\n';
}
