#include "linko_probe/vm_probe.h"

#include <mach/mach.h>
#include <mach/mach_vm.h>
#include <mach/vm_region.h>
#include <mach/vm_statistics.h>

#include <array>
#include <cinttypes>
#include <cstdio>
#include <limits>

namespace {

struct Mappings {
    uint64_t count{};
    uint64_t resident{};
    uint64_t dirty{};
};

// The final bucket holds tags outside the public SDK range.
using Snapshot = std::array<Mappings, VM_MEMORY_COUNT + 1>;

bool ReadMappings(Snapshot& snapshot) {
    mach_vm_address_t address = 0;
    natural_t depth = 0;
    bool observed_mapping = false;
    for (unsigned query = 0; query < 131072; ++query) {
        mach_vm_size_t size = 0;
        vm_region_submap_info_data_64_t info{};
        mach_msg_type_number_t count = VM_REGION_SUBMAP_INFO_COUNT_64;
        const kern_return_t status = mach_vm_region_recurse(
            mach_task_self(), &address, &size, &depth, reinterpret_cast<vm_region_recurse_info_t>(&info), &count);
        if (status == KERN_INVALID_ADDRESS) {
            return observed_mapping;
        }
        if (status != KERN_SUCCESS || count < VM_REGION_SUBMAP_INFO_V0_COUNT_64 || size == 0) {
            return false;
        }
        if (info.is_submap) {
            ++depth;
            continue;
        }
        const auto tag = info.user_tag < VM_MEMORY_COUNT ? info.user_tag : VM_MEMORY_COUNT;
        auto& entry = snapshot.at(tag);
        entry.count += 1;
        entry.resident += static_cast<uint64_t>(info.pages_resident) * vm_page_size;
        entry.dirty += static_cast<uint64_t>(info.pages_dirtied) * vm_page_size;
        observed_mapping = true;
        if (size > std::numeric_limits<mach_vm_address_t>::max() - address) {
            return true;
        }
        address += size;
    }
    return false;
}

int64_t Delta(uint64_t after, uint64_t before) {
    return static_cast<int64_t>(after) - static_cast<int64_t>(before);
}

}  // namespace

extern "C" bool linko_rw04_sample_vm_regions(bool save_baseline) {
    static Snapshot baseline;
    static bool baseline_ready = false;
    Snapshot current{};
    if (!ReadMappings(current)) {
        return false;
    }
    if (save_baseline) {
        baseline = current;
        baseline_ready = true;
        return true;
    }
    if (!baseline_ready) {
        return false;
    }
    std::puts("Loopback VM: stopped mapping comparison at cycles 10 and 100.");
    for (size_t tag = 0; tag < current.size(); ++tag) {
        const auto& before = baseline.at(tag);
        const auto& after = current.at(tag);
        const auto regions = Delta(after.count, before.count);
        const auto resident = Delta(after.resident, before.resident);
        const auto dirty = Delta(after.dirty, before.dirty);
        if (regions || resident || dirty) {
            std::printf("Loopback VM: tag=%zu regions_delta=%" PRId64
                        " resident_delta_bytes=%" PRId64 " dirty_delta_bytes=%" PRId64 "\n",
                        tag, regions, resident, dirty);
        }
    }
    return true;
}
