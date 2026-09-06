#include <qpdf/QPDF.hh>
#include <iostream>
int main() {
    QPDF pdf; pdf.emptyPDF();
    auto direct=QPDFObjectHandle::newInteger(42);
    auto first=pdf.makeIndirectObject(direct);
    std::cout << "direct_alias=" << direct.isSameObjectAs(first) << " og=" << direct.getObjGen().unparse(' ') << '\n';
    auto old=first.getObjGen();
    auto second=pdf.makeIndirectObject(first);
    std::cout << "indirect_alias=" << first.isSameObjectAs(second) << " og=" << first.getObjGen().unparse(' ')
              << " old_slot_og=" << pdf.getObjectByID(old.getObj(),old.getGen()).getObjGen().unparse(' ') << '\n';
    auto reserved=pdf.newReserved();
    auto promoted=pdf.makeIndirectObject(reserved);
    std::cout << "reserved=" << promoted.isReserved() << " alias=" << promoted.isSameObjectAs(reserved) << '\n';
    try { pdf.makeIndirectObject(QPDFObjectHandle()); }
    catch(std::exception const& e) { std::cout << "uninitialized=" << e.what() << '\n'; }
    auto value=QPDFObjectHandle::newInteger(99);
    pdf.replaceObject(old.getObj(),old.getGen(),value);
    auto cached=pdf.getObjectByID(old.getObj(),old.getGen());
    std::cout << "replacement_value_og=" << value.getObjGen().unparse(' ') << " same=" << value.isSameObjectAs(cached) << '\n';
    pdf.makeIndirectObject(value);
    std::cout << "shared_value_og=" << value.getObjGen().unparse(' ') << " cached_og=" << cached.getObjGen().unparse(' ') << '\n';
}
