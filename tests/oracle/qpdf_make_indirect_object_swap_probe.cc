#include <qpdf/QPDF.hh>
#include <qpdf/QPDFEmbeddedFileDocumentHelper.hh>
#include <qpdf/QPDFFileSpecObjectHelper.hh>
#include <iostream>
int main() {
    QPDF pdf; pdf.emptyPDF();
    auto first=pdf.makeIndirectObject(QPDFObjectHandle::newInteger(1));
    auto old=first.getObjGen();
    auto second=pdf.makeIndirectObject(QPDFObjectHandle::newInteger(2));
    auto second_key=second.getObjGen();
    pdf.makeIndirectObject(first);
    pdf.swapObjects(old, second_key);
    std::cout << "first=" << first.getObjGen().unparse(' ') << " second=" << second.getObjGen().unparse(' ') << " values=" << first.getIntValue() << ',' << second.getIntValue() << '\n';
    QPDF foreign; foreign.emptyPDF();
    foreign.getObjectByID(9, 0);
    foreign.makeIndirectObject(first);
    pdf.swapObjects(old, second_key);
    std::cout << "cross_first=" << first.getObjGen().unparse(' ')
              << " first_owner_pdf=" << (first.getOwningQPDF() == &pdf)
              << " second_owner_foreign=" << (second.getOwningQPDF() == &foreign) << '\n';
    QPDF lazy; lazy.emptyPDF();
    auto missing = lazy.getObjectByID(99, 0);
    lazy.makeIndirectObject(missing);
    auto scalar = lazy.makeIndirectObject(QPDFObjectHandle::newInteger(7));
    lazy.swapObjects(QPDFObjGen(99, 0), scalar.getObjGen());
    std::cout << "lazy_first=" << missing.getObjGen().unparse(' ')
              << " lazy_second=" << scalar.getObjGen().unparse(' ')
              << " values=" << missing.getIntValue() << ',' << scalar.isNull() << '\n';
    QPDF source; source.emptyPDF();
    auto fs = QPDFObjectHandle::parse("<< /F (foreign.txt) >>");
    auto owner = QPDFObjectHandle::newDictionary(); owner.replaceKey("/FS",fs);
    source.makeIndirectObject(owner);
    QPDF dest; dest.emptyPDF();
    QPDFEmbeddedFileDocumentHelper helper(dest);
    helper.replaceEmbeddedFile("foreign", QPDFFileSpecObjectHelper(fs));
    std::cout << "embedded=" << helper.getEmbeddedFiles().size() << " direct=" << !fs.isIndirect() << '\n';
}
