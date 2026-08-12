#include <qpdf/Buffer.hh>
#include <qpdf/QPDF.hh>
#include <qpdf/QPDFObjectHandle.hh>

#include <algorithm>
#include <iostream>
#include <memory>
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
        QPDF pdf;
        pdf.emptyPDF();

        auto empty = pdf.newStream();
        require(empty.isIndirect(), "newStream did not create an indirect object");
        require(
            empty.getObjGen().getObj() == 3,
            "newStream object number differs: " + std::to_string(empty.getObjGen().getObj()));
        require(empty.getObjGen().getGen() == 0, "newStream generation differs");
        require(empty.getParsedOffset() == 0, "newStream parsed offset differs");
        require(empty.getDict().getKeys().empty(), "newStream dictionary is not empty");
        require(!empty.getDict().hasKey("/Length"), "empty newStream has /Length");

        empty.getDict().replaceKey("/Marker", QPDFObjectHandle::newInteger(7));
        require(empty.getDict().getKey("/Marker").getIntValue() == 7, "stream dictionary is not live");

        try {
            empty.getRawStreamData();
            throw std::runtime_error("newStream without data unexpectedly piped");
        } catch (std::logic_error const& error) {
            require(
                error.what() == std::string("pipeStreamData called for stream with no data"),
                "newStream no-data error differs");
        }

        auto data = std::make_shared<Buffer>(std::string("abc"));
        auto with_data = pdf.newStream(data);
        require(with_data.isIndirect(), "buffer newStream did not create an indirect object");
        require(with_data.getDict().getKey("/Length").getIntValue() == 3, "buffer length differs");
        auto with_data_raw = with_data.getRawStreamData();
        require(with_data_raw->getSize() == 3, "buffer data size differs");
        require(
            std::equal(
                with_data_raw->getBuffer(),
                with_data_raw->getBuffer() + with_data_raw->getSize(),
                data->getBuffer()),
            "buffer payload differs");

        auto empty_data = pdf.newStream(std::make_shared<Buffer>());
        require(empty_data.isIndirect(), "empty buffer newStream is not indirect");
        require(!empty_data.getDict().hasKey("/Length"), "empty buffer retained /Length");
        require(
            empty_data.getRawStreamData()->getSize() == 0,
            "empty buffer stream did not retain empty data state");

        auto all_objects = pdf.getAllObjects();
        require(all_objects.size() == 5, "new streams changed the expected object count");
        auto is_registered = [&all_objects](QPDFObjectHandle const& stream) {
            return std::any_of(
                all_objects.begin(),
                all_objects.end(),
                [&stream](QPDFObjectHandle const& object) {
                    return object.getObjGen() == stream.getObjGen();
                });
        };
        require(is_registered(with_data), "buffer stream was not registered");
        require(is_registered(empty_data), "empty buffer stream was not registered");

        std::cout << "qpdf new stream probe: ok\n";
        return 0;
    } catch (std::exception const& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
