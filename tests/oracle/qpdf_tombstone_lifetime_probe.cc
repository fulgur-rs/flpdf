#include <fstream>
#include <iomanip>
#include <iostream>
#include <map>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

#include <qpdf/QPDF.hh>
#include <qpdf/QPDFObjectHandle.hh>

namespace
{
void
require(bool condition, std::string const& message)
{
    if (!condition) {
        throw std::runtime_error(message);
    }
}

void
append_object(std::string& pdf, int object, int generation, std::string const& body)
{
    pdf += std::to_string(object) + " " + std::to_string(generation) + " obj\n" + body + "\nendobj\n";
}

std::string
xref_row(long long offset, int generation, char state)
{
    std::ostringstream row;
    row << std::setw(10) << std::setfill('0') << offset << " " << std::setw(5) << generation
        << " " << state << " \n";
    return row.str();
}

std::string
free_row(int generation)
{
    return xref_row(0, generation, 'f');
}

std::string
keys(std::map<QPDFObjGen, QPDFXRefEntry> const& xref)
{
    std::ostringstream result;
    bool first = true;
    for (auto const& [objgen, entry]: xref) {
        (void)entry;
        if (!first) {
            result << ',';
        }
        first = false;
        result << objgen.getObj() << '.' << objgen.getGen();
    }
    return result.str();
}

std::string
keys(std::vector<QPDFObjectHandle> const& objects)
{
    std::ostringstream result;
    bool first = true;
    for (auto const& object: objects) {
        if (!first) {
            result << ',';
        }
        first = false;
        auto objgen = object.getObjGen();
        result << objgen.getObj() << '.' << objgen.getGen();
    }
    return result.str();
}

std::string
free_row_then_later_generation_fixture()
{
    std::string pdf{"%PDF-1.7\n"};
    auto catalog_offset = static_cast<long long>(pdf.size());
    append_object(pdf, 2, 0, "<< /Type /Catalog >>");
    auto previous_object_three_offset = static_cast<long long>(pdf.size());
    append_object(pdf, 3, 1, "99");

    auto previous_xref = static_cast<long long>(pdf.size());
    pdf += "xref\n0 4\n";
    pdf += free_row(65535);
    pdf += free_row(0);
    pdf += xref_row(catalog_offset, 0, 'n');
    pdf += xref_row(previous_object_three_offset, 1, 'n');
    pdf += "trailer\n<< /Size 4 /Root 2 0 R >>\nstartxref\n" + std::to_string(previous_xref) + "\n%%EOF\n";

    auto latest_xref = static_cast<long long>(pdf.size());
    pdf += "xref\n3 1\n";
    pdf += free_row(0);
    pdf += "trailer\n<< /Size 4 /Root 2 0 R /Prev " + std::to_string(previous_xref) + " >>\nstartxref\n" +
        std::to_string(latest_xref) + "\n%%EOF\n";
    return pdf;
}

std::string
damaged_recovery_fixture()
{
    std::string pdf{"%PDF-1.7\n"};
    auto object_two_offset = static_cast<long long>(pdf.size());
    append_object(pdf, 2, 0, "<< /Type /Catalog >>");
    append_object(pdf, 1, 0, "(recovered)");
    append_object(pdf, 3, 0, "99");
    auto xref_offset = static_cast<long long>(pdf.size());
    pdf += "xref\n0 3\n";
    pdf += free_row(65535);
    pdf += xref_row(object_two_offset, 0, 'n');
    pdf += xref_row(object_two_offset, 0, 'n');
    pdf += "trailer\n<< /Size 4 /Root 2 0 R >>\nstartxref\n" + std::to_string(xref_offset) + "\n%%EOF\n";
    return pdf;
}

void
write_fixture(std::string const& path, std::string const& fixture)
{
    std::ofstream output(path, std::ios::binary);
    require(output.good(), "unable to create recovery fixture: " + path);
    output.write(fixture.data(), static_cast<std::streamsize>(fixture.size()));
    require(output.good(), "unable to write recovery fixture: " + path);
}

void
open_memory(QPDF& pdf, std::string const& description, std::string const& fixture)
{
    pdf.setSuppressWarnings(true);
    pdf.setAttemptRecovery(true);
    pdf.processMemoryFile(description.c_str(), fixture.data(), fixture.size());
}
}

int
main(int argc, char* argv[])
{
    try {
        if (argc != 1 && (argc != 3 || std::string(argv[1]) != "--write-fixture")) {
            throw std::runtime_error("usage: qpdf_tombstone_lifetime_probe [--write-fixture PATH]");
        }

        auto recovery_fixture = damaged_recovery_fixture();
        if (argc == 3) {
            write_fixture(argv[2], recovery_fixture);
        }

        QPDF registration;
        open_memory(registration, "free-row registration", free_row_then_later_generation_fixture());
        auto registration_xref = keys(registration.getXRefTable());
        auto registration_all = keys(registration.getAllObjects());
        require(registration_xref == "2.0", "free-row xref observation drifted: " + registration_xref);
        require(
            registration_all == "2.0,3.1",
            "free-row getAllObjects observation drifted: " + registration_all);
        std::cout << "registration.xref=" << registration_xref << '\n';
        std::cout << "registration.all=" << registration_all << '\n';

        QPDF baseline_recovery;
        open_memory(baseline_recovery, "baseline recovery", recovery_fixture);
        auto baseline_before = baseline_recovery.getObject(1, 0).isInitialized();
        require(
            baseline_before,
            "baseline pre-recovery handle was not allocated");
        auto baseline_all = keys(baseline_recovery.getAllObjects()); // calls fixDanglingReferences
        auto baseline_xref = keys(baseline_recovery.getXRefTable());
        auto baseline_object = baseline_recovery.getObject(3, 0);
        require(
            baseline_xref == "1.0,2.0,3.0",
            "baseline recovery xref observation drifted: " + baseline_xref);
        require(
            baseline_all == "1.0,2.0,3.0",
            "baseline recovery getAllObjects observation drifted: " + baseline_all);
        require(
            baseline_object.isInitialized() && baseline_object.getIntValue() == 99,
            "baseline recovery object 3 0 observation drifted");
        std::cout << "baseline.before_recovery.get_1_0_initialized="
                  << (baseline_before ? "true" : "false") << '\n';
        std::cout << "baseline.after_recovery.xref=" << baseline_xref << '\n';
        std::cout << "baseline.after_recovery.all=" << baseline_all << '\n';

        QPDF removed;
        open_memory(removed, "removed-object recovery", recovery_fixture);
        // `QPDF::removeObject` is private and not exported by qpdf 11.9.0.
        // Its public API documents a direct null replacement as the supported
        // way to remove an object from a file.
        removed.replaceObject(3, 0, QPDFObjectHandle::newNull());
        auto removed_before = removed.getObject(1, 0).isInitialized();
        require(removed_before, "pre-recovery handle was not allocated");
        auto removed_all = keys(removed.getAllObjects()); // calls fixDanglingReferences
        auto removed_xref = keys(removed.getXRefTable());
        auto removed_object = removed.getObject(3, 0);
        require(
            removed_xref == "1.0,2.0,3.0",
            "null-replacement xref observation drifted: " + removed_xref);
        require(
            removed_all == "1.0,2.0,3.0",
            "null-replacement getAllObjects observation drifted: " + removed_all);
        require(removed_object.isInitialized(), "null replacement object 3 0 is no longer initialized");
        require(removed_object.isNull(), "recovery replaced the public null-removal object body");
        std::cout << "removal_proxy.before_recovery.get_1_0_initialized="
                  << (removed_before ? "true" : "false") << '\n';
        std::cout << "removal_proxy.after_recovery.xref=" << removed_xref << '\n';
        std::cout << "removal_proxy.after_recovery.all=" << removed_all << '\n';
        std::cout << "removal_proxy.after_recovery.get_3_0_initialized="
                  << (removed_object.isInitialized() ? "true" : "false") << '\n';
        std::cout << "removal_proxy.after_recovery.get_3_0_null="
                  << (removed_object.isNull() ? "true" : "false") << '\n';

        QPDF replaced;
        open_memory(replaced, "replacement recovery", recovery_fixture);
        // Exercise the same public sequence as both Rust mutation-route
        // regressions: observable removal, same-generation replacement, then
        // different-generation replacement before forcing recovery.
        replaced.replaceObject(3, 0, QPDFObjectHandle::newNull());
        replaced.replaceObject(3, 0, QPDFObjectHandle::newInteger(70));
        require(
            replaced.getObject(3, 0).getIntValue() == 70,
            "same-generation replacement was not visible before recovery");
        replaced.replaceObject(3, 1, QPDFObjectHandle::newInteger(71));
        auto replacement_before = replaced.getObject(1, 0).isInitialized();
        require(replacement_before, "pre-recovery handle was not allocated");
        auto replacement_all = keys(replaced.getAllObjects()); // calls fixDanglingReferences
        auto replacement_xref = keys(replaced.getXRefTable());
        auto replacement_source = replaced.getObject(3, 0);
        auto replacement_object = replaced.getObject(3, 1);
        require(
            replacement_xref == "1.0,2.0,3.0",
            "generation-replacement xref observation drifted: " + replacement_xref);
        require(
            replacement_all == "1.0,2.0,3.0,3.1",
            "generation-replacement getAllObjects observation drifted: " + replacement_all);
        require(
            replacement_source.isInitialized() && replacement_source.getIntValue() == 70,
            "same-generation replacement object 3 0 observation drifted");
        require(
            replacement_object.isInitialized() && replacement_object.getIntValue() == 71,
            "generation-replacement object 3 1 observation drifted");
        std::cout << "replacement.before_recovery.get_1_0_initialized="
                  << (replacement_before ? "true" : "false") << '\n';
        std::cout << "replacement.after_recovery.xref=" << replacement_xref << '\n';
        std::cout << "replacement.after_recovery.all=" << replacement_all << '\n';
        std::cout << "replacement.after_recovery.get_3_0_initialized="
                  << (replacement_source.isInitialized() ? "true" : "false") << '\n';
        std::cout << "replacement.after_recovery.get_3_0_value=" << replacement_source.getIntValue()
                  << '\n';
        std::cout << "replacement.after_recovery.get_3_1_initialized="
                  << (replacement_object.isInitialized() ? "true" : "false") << '\n';
        std::cout << "replacement.after_recovery.get_3_1_value=" << replacement_object.getIntValue()
                  << '\n';
        return 0;
    } catch (std::exception const& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
