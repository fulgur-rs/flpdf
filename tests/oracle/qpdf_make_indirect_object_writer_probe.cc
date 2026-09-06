// Usage: probe OUTPUT.pdf; qpdf 11.9.0 golden for a retained promotion alias.
#include <qpdf/QPDF.hh>
#include <qpdf/QPDFWriter.hh>
int main(int argc, char** argv)
{
    if (argc != 2) {
        return 2;
    }
    QPDF pdf;
    pdf.emptyPDF();
    auto source = QPDFObjectHandle::newArray();
    source.appendItem(QPDFObjectHandle::newInteger(1));
    auto promoted = pdf.makeIndirectObject(source);
    pdf.getTrailer().replaceKey("/Promoted", promoted);
    source.appendItem(QPDFObjectHandle::newInteger(42));
    QPDFWriter writer(pdf, argv[1]);
    writer.setStaticID(true);
    writer.write();
}
