#include <qpdf/QPDF.hh>
#include <qpdf/JSON.hh>
#include <iostream>
#include <limits>
int main(int argc, char** argv) {
    QPDF pdf; pdf.emptyPDF();
    auto reserved = pdf.newReserved().shallowCopy();
    auto result = pdf.makeIndirectObject(reserved);
    std::cout << "direct_reserved=" << result.isReserved() << " alias=" << result.isSameObjectAs(reserved) << '\n';
    QPDFObjectHandle destroyed;
    { QPDF other; other.emptyPDF(); destroyed = other.makeIndirectObject(QPDFObjectHandle::newInteger(8)); }
    result = pdf.makeIndirectObject(destroyed);
    std::cout << "destroyed=" << result.getTypeCode() << " alias=" << result.isSameObjectAs(destroyed) << '\n';
    auto missing = pdf.getObjectByID(99, 0);
    result = pdf.makeIndirectObject(missing);
    std::cout << "missing=" << result.getObjGen().unparse(' ') << " warnings=" << pdf.getWarnings().size() << '\n';
    std::cout << "json_ref=" << result.getJSON(2, false).unparse()
              << " json_value=" << result.getJSON(2, true).unparse() << '\n';
    QPDF limit; limit.emptyPDF();
    limit.getObjectByID(std::numeric_limits<int>::max()-1, 0);
    auto last = limit.makeIndirectObject(QPDFObjectHandle::newNull());
    std::cout << "last=" << last.getObjGen().unparse(' ') << '\n';
    try { limit.makeIndirectObject(QPDFObjectHandle()); }
    catch (std::exception const& e) { std::cout << "uninitialized=" << e.what() << '\n'; }
    try { limit.makeIndirectObject(QPDFObjectHandle::newNull()); }
    catch (std::exception const& e) { std::cout << "overflow=" << e.what() << '\n'; }
    if (argc == 2) {
        QPDF historical; historical.processFile(argv[1]);
        std::cout << "historical_count=" << historical.getObjectCount();
        auto created = historical.makeIndirectObject(QPDFObjectHandle::newInteger(7));
        std::cout << " next=" << created.getObjGen().unparse(' ') << '\n';
    }
}
