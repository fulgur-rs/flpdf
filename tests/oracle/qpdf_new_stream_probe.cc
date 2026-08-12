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
        require(empty.getOwningQPDF() == &pdf, "newStream owner differs");
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
        require(with_data.getObjGen().getObj() == 4, "buffer newStream object number differs");
        require(with_data.getObjGen().getGen() == 0, "buffer newStream generation differs");
        require(with_data.getDict().getKey("/Length").getIntValue() == 3, "buffer length differs");
        auto with_data_raw = with_data.getRawStreamData();
        require(with_data_raw->getSize() == 3, "buffer data size differs");
        require(
            std::equal(
                with_data_raw->getBuffer(),
                with_data_raw->getBuffer() + with_data_raw->getSize(),
                data->getBuffer()),
            "buffer payload differs");
        data->getBuffer()[0] = 'z';
        auto mutated_data_raw = with_data.getRawStreamData();
        auto mutated_payload = std::string("zbc");
        require(
            mutated_data_raw->getSize() == mutated_payload.size(),
            "mutated buffer data size differs");
        require(
            std::equal(
                mutated_data_raw->getBuffer(),
                mutated_data_raw->getBuffer() + mutated_data_raw->getSize(),
                mutated_payload.begin()),
            "buffer mutation was not retained by stream");
        require(with_data.getOwningQPDF() == &pdf, "buffer newStream owner differs");

        auto empty_data_buffer = std::make_shared<Buffer>();
        std::weak_ptr<Buffer> empty_data_ownership = empty_data_buffer;
        auto empty_data = pdf.newStream(empty_data_buffer);
        empty_data_buffer.reset();
        require(empty_data.isIndirect(), "empty buffer newStream is not indirect");
        require(empty_data.getObjGen().getObj() == 5, "empty buffer object number differs");
        require(empty_data.getObjGen().getGen() == 0, "empty buffer generation differs");
        require(
            !empty_data_ownership.expired(),
            "empty buffer newStream did not retain the supplied allocation");
        require(empty_data.getOwningQPDF() == &pdf, "empty buffer newStream owner differs");
        require(!empty_data.getDict().hasKey("/Length"), "empty buffer retained /Length");
        require(
            empty_data.getRawStreamData()->getSize() == 0,
            "empty buffer stream did not retain empty data state");

        auto all_objects = pdf.getAllObjects();
        require(all_objects.size() == 5, "new streams changed the expected object count");
        auto registered_handle = [&all_objects](QPDFObjectHandle const& stream) {
            auto it = std::find_if(
                all_objects.begin(),
                all_objects.end(),
                [&stream](QPDFObjectHandle const& object) {
                    return object.getObjGen() == stream.getObjGen();
                });
            require(it != all_objects.end(), "stream was not registered");
            return *it;
        };
        auto registered_with_data = registered_handle(with_data);
        auto registered_empty_data = registered_handle(empty_data);
        registered_with_data.getDict().replaceKey(
            "/Marker", QPDFObjectHandle::newInteger(11));
        require(
            with_data.getDict().getKey("/Marker").getIntValue() == 11,
            "registered buffer stream is disconnected from returned handle");
        registered_empty_data.getDict().replaceKey(
            "/Marker", QPDFObjectHandle::newInteger(13));
        require(
            empty_data.getDict().getKey("/Marker").getIntValue() == 13,
            "registered empty buffer stream is disconnected from returned handle");

        std::cout << "qpdf new stream probe: ok\n";
        return 0;
    } catch (std::exception const& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
