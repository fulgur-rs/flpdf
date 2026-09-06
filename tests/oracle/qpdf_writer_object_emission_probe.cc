#include <qpdf/QPDF.hh>
#include <qpdf/QPDFWriter.hh>
#include <iostream>
int main() {
    QPDF pdf;
    pdf.emptyPDF();
    auto root = pdf.getRoot();
    QPDFWriter writer(pdf);
    writer.setOutputMemory();
    writer.setObjectStreamMode(qpdf_o_disable);
    writer.setStaticID(true);
    writer.registerProgressReporter(std::make_shared<QPDFWriter::FunctionProgressReporter>(
        [&](int percent) {
            std::cerr << percent << '\n';
            if (percent == 0) root.replaceKey("/ProgressProbe", QPDFObjectHandle::newInteger(42));
        }));
    writer.write();
    auto buffer = writer.getBufferSharedPointer();
    std::string bytes(reinterpret_cast<char const*>(buffer->getBuffer()), buffer->getSize());
    std::cout << "root_contains_progress_mutation=" << (bytes.find("/ProgressProbe 42") != std::string::npos) << '\n';
}
