//! R85 disk-floor decision over caller-supplied filesystem observations.

/// Space available to an unprivileged process and total filesystem capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilesystemSpace {
    pub available_bytes: u64,
    pub total_bytes: u64,
}

/// The least whole number of bytes that meets both 5 GiB and 5% of capacity.
pub fn floor_bytes(total_bytes: u64) -> u64 {
    const FIVE_GIB: u64 = 5 * 1024 * 1024 * 1024;
    FIVE_GIB.max(total_bytes.div_ceil(20))
}

/// Whether either the data or project workspace filesystem is below its floor.
///
/// Equality meets R85. The caller owns probing and freshness, admission checks,
/// and attention-item recovery hysteresis (R82).
pub fn below_disk_floor(data: FilesystemSpace, workspaces: FilesystemSpace) -> bool {
    data.available_bytes < floor_bytes(data.total_bytes)
        || workspaces.available_bytes < floor_bytes(workspaces.total_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIVE_GIB: u64 = 5 * 1024 * 1024 * 1024;

    fn space(available_bytes: u64, total_bytes: u64) -> FilesystemSpace {
        FilesystemSpace {
            available_bytes,
            total_bytes,
        }
    }

    #[test]
    fn absolute_floor_wins_on_small_filesystems() {
        for total in [0, FIVE_GIB - 1, FIVE_GIB, 20 * FIVE_GIB - 1, 20 * FIVE_GIB] {
            assert_eq!(floor_bytes(total), FIVE_GIB, "total={total}");
        }
    }

    #[test]
    fn percentage_floor_rounds_up_to_a_whole_byte() {
        for remainder in 1..20 {
            assert_eq!(floor_bytes(20 * FIVE_GIB + remainder), FIVE_GIB + 1);
        }
        assert_eq!(floor_bytes(40 * FIVE_GIB), 2 * FIVE_GIB);
        assert_eq!(floor_bytes(40 * FIVE_GIB + 1), 2 * FIVE_GIB + 1);
    }

    #[test]
    fn percentage_floor_is_exact_near_u64_maximum() {
        for total in u64::MAX - 39..=u64::MAX {
            let floor = floor_bytes(total);
            assert!(u128::from(floor) * 20 >= u128::from(total));
            assert!(u128::from(floor - 1) * 20 < u128::from(total));
        }
        assert_eq!(floor_bytes(u64::MAX), 922_337_203_685_477_581);
    }

    #[test]
    fn absolute_floor_equality_allows_both_subjects() {
        let at_floor = space(FIVE_GIB, 10 * FIVE_GIB);
        assert!(!below_disk_floor(at_floor, at_floor));
        assert!(!below_disk_floor(
            space(FIVE_GIB + 1, 10 * FIVE_GIB),
            at_floor
        ));
    }

    #[test]
    fn either_subject_below_absolute_floor_blocks() {
        let healthy = space(FIVE_GIB, 10 * FIVE_GIB);
        let low = space(FIVE_GIB - 1, 10 * FIVE_GIB);
        assert!(below_disk_floor(low, healthy));
        assert!(below_disk_floor(healthy, low));
        assert!(below_disk_floor(low, low));
        assert!(below_disk_floor(space(0, 0), healthy));
    }

    #[test]
    fn each_subject_uses_its_own_percentage_floor() {
        let small = space(FIVE_GIB, 10 * FIVE_GIB);
        let large = space(2 * FIVE_GIB, 40 * FIVE_GIB);
        let low = space(2 * FIVE_GIB - 1, 40 * FIVE_GIB);
        assert!(!below_disk_floor(small, large));
        assert!(!below_disk_floor(large, small));
        assert!(below_disk_floor(small, low));
        assert!(below_disk_floor(low, small));
    }

    #[test]
    fn fractional_percentage_and_maximum_boundaries_apply_to_either_subject() {
        let healthy = space(FIVE_GIB, 10 * FIVE_GIB);
        for (total, floor) in [
            (20 * FIVE_GIB + 1, FIVE_GIB + 1),
            (u64::MAX, 922_337_203_685_477_581),
        ] {
            let at_floor = space(floor, total);
            let below = space(floor - 1, total);
            let above = space(floor + 1, total);
            assert!(!below_disk_floor(healthy, at_floor));
            assert!(!below_disk_floor(at_floor, healthy));
            assert!(!below_disk_floor(above, above));
            assert!(below_disk_floor(healthy, below));
            assert!(below_disk_floor(below, healthy));
        }
    }
}
