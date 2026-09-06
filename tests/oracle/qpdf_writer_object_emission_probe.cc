#include <qpdf/QPDF.hh>
#include <qpdf/QPDFWriter.hh>
#include <iostream>

int main() {
    for (bool replace_stream: {false, true}) {
        QPDF pdf;
        pdf.emptyPDF();
        auto root = pdf.getRoot();
        auto stream = pdf.newStream("before");
        if (replace_stream) {
            root.replaceKey("/ProbeStream", stream);
        }
        QPDFWriter writer(pdf);
        writer.setOutputMemory();
        writer.setObjectStreamMode(qpdf_o_disable);
        writer.setCompressStreams(false);
        writer.setStaticID(true);
        writer.registerProgressReporter(std::make_shared<QPDFWriter::FunctionProgressReporter>(
            [&](int percent) {
                if (percent == 0) {
                    if (replace_stream) {
                        stream.replaceStreamData("after", QPDFObjectHandle(), QPDFObjectHandle());
                    } else {
                        root.replaceKey("/ProgressProbe", QPDFObjectHandle::newInteger(42));
                    }
                }
            }));
        writer.write();
        auto buffer = writer.getBufferSharedPointer();
        std::string bytes(reinterpret_cast<char const*>(buffer->getBuffer()), buffer->getSize());
        if (replace_stream) {
            std::cout << "stream_contains_replacement="
                      << (bytes.find("stream\nafter") != std::string::npos) << '\n';
        } else {
            std::cout << "root_contains_progress_mutation="
                      << (bytes.find("/ProgressProbe 42") != std::string::npos) << '\n';
        }
    }
}
