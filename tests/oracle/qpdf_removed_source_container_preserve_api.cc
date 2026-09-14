#include <qpdf/QPDF.hh>
#include <qpdf/QPDFWriter.hh>

int main(int argc, char* argv[])
{
    if (argc != 3) {
        return 2;
    }

    QPDF pdf;
    pdf.processFile(argv[1]);

    // This mutation cannot be expressed by qpdf's command-line interface:
    // retain a handle for 1 0 while replacing the newer 1 1 cache entry.
    QPDFObjectHandle source_container = pdf.getObjectByID(1, 0);
    pdf.getRoot().replaceKey("/SourceObjStm", source_container);
    pdf.replaceObject(1, 1, QPDFObjectHandle::newNull());

    QPDFWriter writer(pdf, argv[2]);
    writer.setObjectStreamMode(qpdf_o_preserve);
    writer.setStaticID(true);
    writer.write();
}
