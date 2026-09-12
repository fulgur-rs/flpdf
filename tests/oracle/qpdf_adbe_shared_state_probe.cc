// Build with pinned 11.9.0 headers and libqpdf (or -lqpdf with its dev package).
// Run: probe fixture.pdf {standard,linearize,suppressed} {0,8}
//            {ok,fail,first,second,trace}
// first/second use one-page.pdf: seven body objects, second-pass root at 58%.
// Oracle: qpdf 11.9.0 QPDFWriter.cc:1347-1435 (shared Extensions mutation).
#include <qpdf/QPDF.hh>
#include <qpdf/QPDFWriter.hh>
#include <qpdf/Pipeline.hh>
#include <iostream>
#include <stdexcept>
#include <string>

class FinishFailure: public Pipeline
{
  public:
    FinishFailure(): Pipeline("finish failure", nullptr) {}
    void write(unsigned char const*, size_t) override {}
    void finish() override { throw std::runtime_error("output failure"); }
};

int main(int argc, char** argv)
{
    if (argc != 5) { return 2; }
    QPDF pdf;
    pdf.processFile(argv[1]);
    auto root = pdf.getRoot();
    bool suppressed = std::string(argv[2]) == "suppressed";
    auto extensions = suppressed ? root.getKey("/Extensions") : QPDFObjectHandle::parse(
        "<< /ADBE << /BaseVersion /1.7 /ExtensionLevel 3 >> /ACME 1 >>");
    if (!suppressed) { root.replaceKey("/Extensions", extensions); }
    bool fail = std::string(argv[4]) == "fail";
    FinishFailure sink;
    QPDFWriter writer(pdf);
    writer.setStaticID(true);
    writer.setExtraHeaderText("% ADBE probe\n");
    writer.setLinearization(std::string(argv[2]) == "linearize");
    writer.forcePDFVersion("1.7", std::stoi(argv[3]));
    if (std::string(argv[4]) == "trace") {
        writer.registerProgressReporter(std::make_shared<QPDFWriter::FunctionProgressReporter>(
            [&](int percent) { std::cout << "progress " << percent << "\n"; }));
    }
    if (std::string(argv[4]) == "first" || std::string(argv[4]) == "second") {
        int fail_at = std::string(argv[4]) == "first" ? 0 : 58;
        writer.registerProgressReporter(std::make_shared<QPDFWriter::FunctionProgressReporter>(
            [fail_at](int percent) {
                if (percent == fail_at) { throw std::runtime_error("callback failure"); }
            }));
    }
    if (suppressed) {
        writer.forcePDFVersion("1.4.2", 0);
        writer.setObjectStreamMode(qpdf_o_generate);
    }
    if (fail) { writer.setOutputPipeline(&sink); }
    else { writer.setOutputMemory(); }
    try { writer.write(); std::cout << "ok\n"; }
    catch (std::runtime_error const& e) { std::cout << e.what() << "\n"; }
    std::cout << root.getKey("/Extensions").isSameObjectAs(extensions) << "\n"
              << root.getKey("/Extensions").unparse() << "\n";
}
