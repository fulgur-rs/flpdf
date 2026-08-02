#include <qpdf/QPDF.hh>
#include <qpdf/QPDFObjectHandle.hh>

#include <exception>
#include <iostream>

int
main(int argc, char* argv[])
{
    if (argc != 2) {
        std::cerr << "usage: qpdf_objecthandle_dereference_probe INPUT.pdf\n";
        return 2;
    }

    try {
        QPDF qpdf;
        qpdf.processFile(argv[1]);

        auto root = qpdf.getTrailer().getKey("/Root");
        std::cout << "root-indirect\t" << root.isIndirect() << '\n';
        std::cout << "root-dictionary\t" << root.isDictionary() << '\n';
        std::cout << "root-has-pages\t" << root.hasKey("/Pages") << '\n';

        auto pages = root.getKey("/Pages");
        std::cout << "root-still-indirect\t" << root.isIndirect() << '\n';
        std::cout << "pages-indirect\t" << pages.isIndirect() << '\n';
        std::cout << "pages-dictionary\t" << pages.isDictionary() << '\n';
        std::cout << "pages-still-indirect\t" << pages.isIndirect() << '\n';
        return 0;
    } catch (std::exception const& e) {
        std::cerr << e.what() << '\n';
        return 2;
    }
}
