#include <qpdf/QPDF.hh>
#include <qpdf/QPDFWriter.hh>
#include <cstdlib>
#include <iostream>
#include <string>

// Generate the qpdf 11.9.0 oracle for the one writer input that the qpdf CLI
// cannot express: QPDFWriter::setExtraHeaderText (QPDFWriter.cc:269) has no
// QPDFJob/command-line binding, so the extra-header-text route can only be
// driven through the C++ API.
//
// The option set mirrors `qpdf --qdf --object-streams=generate --static-id`,
// which QDF mode keeps free of DEFLATE output (doWriteSetup clears
// compress_streams, QPDFWriter.cc:2078-2088).
//
// usage: qpdf-extra-header-generate-api INPUT.pdf OUTPUT.pdf HEADER_TEXT
int
main(int argc, char** argv)
{
    if (argc != 4) {
        std::cerr << "usage: " << argv[0] << " INPUT.pdf OUTPUT.pdf HEADER_TEXT" << std::endl;
        return 2;
    }
    QPDF pdf;
    pdf.processFile(argv[1]);
    QPDFWriter w(pdf, argv[2]);
    w.setStaticID(true);
    w.setQDFMode(true);
    w.setObjectStreamMode(qpdf_o_generate);
    w.setExtraHeaderText(std::string(argv[3]));
    w.write();
    return 0;
}
