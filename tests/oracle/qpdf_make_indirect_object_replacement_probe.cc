#include <qpdf/QPDF.hh>
#include <iostream>
int main() {
    QPDF pdf; pdf.emptyPDF();
    auto target = pdf.makeIndirectObject(QPDFObjectHandle::newInteger(1));
    auto old = target.getObjGen();
    auto shared = QPDFObjectHandle::newInteger(2);
    pdf.replaceObject(old, shared);
    pdf.makeIndirectObject(shared);
    std::cout << "before=" << shared.getObjGen().unparse(' ');
    pdf.replaceObject(old, QPDFObjectHandle::newInteger(3));
    std::cout << " after=" << shared.getObjGen().unparse(' ') << " target=" << target.getObjGen().unparse(' ') << '\n';
    auto absent = QPDFObjectHandle::newInteger(9);
    pdf.replaceObject(QPDFObjGen(100, 0), absent);
    std::cout << "absent_alias=" << absent.isSameObjectAs(pdf.getObjectByID(100,0)) << '\n';
    auto surviving = QPDFObjectHandle::newInteger(99);
    {
        QPDF dying; dying.emptyPDF();
        auto target = dying.makeIndirectObject(QPDFObjectHandle::newInteger(1));
        dying.replaceObject(target.getObjGen(), surviving);
    }
    std::cout << "surviving_direct=" << !surviving.isIndirect()
              << " owner_null=" << (surviving.getOwningQPDF() == nullptr)
              << " value=" << surviving.getIntValue() << '\n';
}
