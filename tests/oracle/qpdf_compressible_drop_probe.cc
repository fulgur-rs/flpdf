#include <qpdf/QPDF.hh>
#include <qpdf/QPDFWriter.hh>
#include <functional>
#include <iostream>
struct DropProvider: QPDFObjectHandle::StreamDataProvider {
    std::function<void()> action;
    explicit DropProvider(std::function<void()> action): action(std::move(action)) {}
    ~DropProvider() override { action(); }
};
int main(int argc, char** argv) {
    if (argc != 2) return 2;
    for (bool grow: {false,true}) {
        QPDF pdf; pdf.processFile(argv[1]);
        QPDFObjectHandle old;
        for (auto object: pdf.getAllObjects()) if (object.isStream()) { old=object; break; }
        auto og=old.getObjGen();
        pdf.replaceObject(og.getObj(),og.getGen()+1,QPDFObjectHandle::newInteger(42));
        auto pending=QPDFObjectHandle::newArray();
        pdf.getRoot().replaceKey("/ZPending",pending);
        auto armed=std::make_shared<bool>(true);
        bool fired=false;
        old.replaceStreamData(std::make_shared<DropProvider>([&,pending,old,og,armed]() mutable {
            if (!*armed) return;
            fired=true;
            auto cached=pdf.getObjectByID(og.getObj(),og.getGen());
            std::cout << "xref_absent=" << !pdf.getXRefTable().count(og)
                      << " cache_same=" << cached.isSameObjectAs(old)
                      << " null=" << cached.isNull() << " indirect=" << cached.isIndirect() << '\n';
            if (grow) pending.appendItem(pdf.makeIndirectObject(QPDFObjectHandle::newInteger(7)));
        }),QPDFObjectHandle(),QPDFObjectHandle());
        try { QPDFWriter w(pdf); w.setOutputMemory(); w.setObjectStreamMode(qpdf_o_generate); w.write(); std::cout<<"write=success\n"; }
        catch (std::exception const& e) { std::cout << "error=" << e.what() << '\n'; }
        *armed=false;
        std::cout<<"fired="<<fired<<'\n';
    }
}
