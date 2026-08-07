#include <qpdf/QPDF.hh>
#include <qpdf/QPDFObjectHandle.hh>

#include <iostream>
#include <stdexcept>
#include <string>

namespace
{
void require(bool condition, std::string const& message)
{
    if (!condition) {
        throw std::runtime_error(message);
    }
}
}

int main()
{
    try {
        auto direct = QPDFObjectHandle::newDictionary();
        direct.replaceKey("/Value", QPDFObjectHandle::newInteger(1));
        auto original_clone = direct;

        QPDF first;
        first.emptyPDF();
        auto first_indirect = first.makeIndirectObject(direct);
        require(direct.isSameObjectAs(original_clone), "direct clone identity changed");
        require(direct.isSameObjectAs(first_indirect), "promotion cloned QPDFObject");
        require(direct.isIndirect() && original_clone.isIndirect(), "promotion metadata was not shared");
        require(direct.getObjGen() == first_indirect.getObjGen(), "promoted ObjGen differs");
        auto first_objgen = first_indirect.getObjGen();
        require(direct.getOwningQPDF() == &first, "first promotion did not install first owner");
        require(original_clone.getOwningQPDF() == &first, "clone did not observe first owner");
        require(first_indirect.getOwningQPDF() == &first, "indirect handle did not observe first owner");

        original_clone.replaceKey("/Value", QPDFObjectHandle::newInteger(2));
        require(first_indirect.getKey("/Value").getIntValue() == 2, "direct-to-indirect mutation was not visible");
        first_indirect.replaceKey("/Value", QPDFObjectHandle::newInteger(3));
        require(direct.getKey("/Value").getIntValue() == 3, "indirect-to-direct mutation was not visible");

        auto repeated = first.makeIndirectObject(direct);
        require(repeated.isSameObjectAs(direct), "repeat promotion changed identity");
        require(direct.getObjGen() == repeated.getObjGen(), "repeat promotion did not install latest ObjGen");
        require(repeated.getObjGen() != first_objgen, "repeat promotion retained the prior ObjGen");
        require(direct.getOwningQPDF() == &first, "repeat promotion changed owner unexpectedly");
        require(repeated.getOwningQPDF() == &first, "repeat promotion did not retain first owner");
        auto repeated_objgen = repeated.getObjGen();

        {
            QPDF second;
            second.emptyPDF();
            auto cross_document = second.makeIndirectObject(direct);
            require(cross_document.isSameObjectAs(direct), "cross-document promotion changed identity");
            require(direct.getObjGen() == cross_document.getObjGen(), "cross-document metadata was not latest");
            require(cross_document.getObjGen() != repeated_objgen, "cross-document promotion retained the prior ObjGen");
            require(direct.getOwningQPDF() == &second, "cross-document promotion did not install second owner");
            require(cross_document.getOwningQPDF() == &second, "cross-document handle did not observe second owner");
        }

        require(!direct.isIndirect(), "latest owner drop retained ObjGen");
        require(
            std::string(direct.getTypeName()) == "destroyed",
            "latest owner drop retained a live value");
        std::cout << "qpdf uniform object identity probe: ok\n";
        return 0;
    } catch (std::exception const& e) {
        std::cerr << e.what() << '\n';
        return 1;
    }
}
