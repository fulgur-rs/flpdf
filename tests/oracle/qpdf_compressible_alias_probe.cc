#include <qpdf/QPDF.hh>
#include <qpdf/QPDFWriter.hh>
#include <iostream>
int main(int argc, char** argv) {
    if (argc != 2) return 2;
    QPDF pdf;
    pdf.processFile(argv[1]);
    auto old = pdf.getObjectByID(3, 0);
    auto current = pdf.getObjectByID(3, 1);
    std::cout << "before_old_indirect=" << old.isIndirect() << '\n';
    QPDFWriter writer(pdf);
    writer.setOutputMemory();
    writer.setObjectStreamMode(qpdf_o_generate);
    writer.setStaticID(true);
    writer.write();
    std::cout << "after_old_indirect=" << old.isIndirect() << '\n';
    std::cout << "after_old_null=" << old.isNull() << '\n';
    std::cout << "after_old_objgen=" << old.getObjGen().unparse(' ') << '\n';
    std::cout << "current_objgen=" << current.getObjGen().unparse(' ') << '\n';
}
