#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PerformanceResources {
    pub physical_ram_bytes: u64,
    pub dedicated_vram_bytes: Option<u64>,
    pub logical_cpu_count: usize,
    pub gpu_adapter_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerformanceSettingsResolved {
    pub l1_vram_cache_max_mib: u16,
    pub l2_ram_cache_max_mib: u16,
    pub background_worker_count: usize,
}

pub const PERFORMANCE_CACHE_MIN_MIB: u16 = 256;
pub const PERFORMANCE_BG_MIN_WORKERS: u16 = 2;

fn bytes_to_mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

fn cap_mib(value: u64) -> u16 {
    value.min(u16::MAX as u64) as u16
}

pub fn mib_to_bytes(mib: u16) -> usize {
    (mib as usize).saturating_mul(1024 * 1024)
}

pub fn split_mib_evenly(total_mib: u16) -> (u16, u16) {
    let future = total_mib / 2;
    (future, total_mib.saturating_sub(future))
}

pub fn power_of_two_candidates(min_mib: u16, upper_mib: u16) -> Vec<u16> {
    let upper_mib = upper_mib.max(min_mib);
    let mut candidates = Vec::new();
    let mut value = min_mib;
    loop {
        candidates.push(value);
        if value > upper_mib / 2 {
            break;
        }
        let Some(next) = value.checked_mul(2) else {
            break;
        };
        if next > upper_mib {
            break;
        }
        value = next;
    }
    candidates
}

pub fn floor_to_power_of_two_candidate(min_mib: u16, upper_mib: u16, target_mib: u16) -> u16 {
    let target_mib = target_mib.max(min_mib);
    power_of_two_candidates(min_mib, upper_mib)
        .into_iter()
        .rfind(|candidate| *candidate <= target_mib)
        .unwrap_or(min_mib)
}

pub fn even_candidates(min_value: u16, upper_value: u16) -> Vec<u16> {
    let upper_value = upper_value.max(min_value);
    let mut candidates = Vec::new();
    let mut value = if min_value % 2 == 0 {
        min_value
    } else {
        min_value.saturating_add(1)
    };
    if value < min_value {
        value = min_value;
    }
    while value <= upper_value {
        candidates.push(value);
        let Some(next) = value.checked_add(2) else {
            break;
        };
        value = next;
    }
    if candidates.is_empty() {
        candidates.push(min_value);
    }
    candidates
}

pub fn floor_to_even_candidate(min_value: u16, upper_value: u16, target_value: u16) -> u16 {
    let target_value = target_value.max(min_value);
    even_candidates(min_value, upper_value)
        .into_iter()
        .rfind(|candidate| *candidate <= target_value)
        .unwrap_or(min_value)
}

impl PerformanceResources {
    pub fn physical_ram_mib(&self) -> u64 {
        bytes_to_mib(self.physical_ram_bytes)
    }

    pub fn dedicated_vram_mib(&self) -> Option<u64> {
        self.dedicated_vram_bytes.map(bytes_to_mib)
    }

    pub fn l1_normal_upper_mib(&self) -> u16 {
        cap_mib(self.dedicated_vram_mib().unwrap_or(0) / 2).max(PERFORMANCE_CACHE_MIN_MIB)
    }

    pub fn l2_normal_upper_mib(&self) -> u16 {
        cap_mib(self.physical_ram_mib() / 2).max(PERFORMANCE_CACHE_MIN_MIB)
    }

    pub fn l1_danger_upper_mib(&self) -> u16 {
        self.dedicated_vram_mib()
            .map(|mib| cap_mib((mib.saturating_mul(3)) / 4))
            .unwrap_or(PERFORMANCE_CACHE_MIN_MIB)
            .max(PERFORMANCE_CACHE_MIN_MIB)
    }

    pub fn l2_danger_upper_mib(&self) -> u16 {
        cap_mib((self.physical_ram_mib().saturating_mul(3)) / 4).max(PERFORMANCE_CACHE_MIN_MIB)
    }

    pub fn bg_normal_upper_workers(&self) -> u16 {
        let half = self.logical_cpu_count / 2;
        let upper = u16::try_from(half).unwrap_or(u16::MAX);
        upper.max(PERFORMANCE_BG_MIN_WORKERS)
    }

    pub fn bg_danger_upper_workers(&self) -> u16 {
        let upper = (self.logical_cpu_count.saturating_mul(3)) / 4;
        let upper = u16::try_from(upper).unwrap_or(u16::MAX);
        upper.max(1)
    }

    pub fn l1_normal_candidates(&self) -> Vec<u16> {
        power_of_two_candidates(PERFORMANCE_CACHE_MIN_MIB, self.l1_normal_upper_mib())
    }

    pub fn l2_normal_candidates(&self) -> Vec<u16> {
        power_of_two_candidates(PERFORMANCE_CACHE_MIN_MIB, self.l2_normal_upper_mib())
    }

    pub fn bg_normal_candidates(&self) -> Vec<u16> {
        even_candidates(PERFORMANCE_BG_MIN_WORKERS, self.bg_normal_upper_workers())
    }

    pub fn l1_default_mib(&self) -> u16 {
        self.dedicated_vram_mib()
            .map(|mib| {
                floor_to_power_of_two_candidate(
                    PERFORMANCE_CACHE_MIN_MIB,
                    self.l1_normal_upper_mib(),
                    cap_mib(mib / 8).max(PERFORMANCE_CACHE_MIN_MIB),
                )
            })
            .unwrap_or(PERFORMANCE_CACHE_MIN_MIB)
    }

    pub fn l2_default_mib(&self) -> u16 {
        floor_to_power_of_two_candidate(
            PERFORMANCE_CACHE_MIN_MIB,
            self.l2_normal_upper_mib(),
            cap_mib(self.physical_ram_mib() / 8).max(PERFORMANCE_CACHE_MIN_MIB),
        )
    }

    pub fn bg_default_workers(&self) -> u16 {
        let target = u16::try_from(self.logical_cpu_count / 4).unwrap_or(u16::MAX);
        floor_to_even_candidate(
            PERFORMANCE_BG_MIN_WORKERS,
            self.bg_normal_upper_workers(),
            target.max(PERFORMANCE_BG_MIN_WORKERS),
        )
    }

    pub fn normalize_l1_mib(&self, value_mib: u16, danger_zone_enabled: bool) -> u16 {
        if danger_zone_enabled {
            value_mib.clamp(PERFORMANCE_CACHE_MIN_MIB, self.l1_danger_upper_mib())
        } else {
            floor_to_power_of_two_candidate(
                PERFORMANCE_CACHE_MIN_MIB,
                self.l1_normal_upper_mib(),
                value_mib,
            )
        }
    }

    pub fn normalize_l2_mib(&self, value_mib: u16, danger_zone_enabled: bool) -> u16 {
        if danger_zone_enabled {
            value_mib.clamp(PERFORMANCE_CACHE_MIN_MIB, self.l2_danger_upper_mib())
        } else {
            floor_to_power_of_two_candidate(
                PERFORMANCE_CACHE_MIN_MIB,
                self.l2_normal_upper_mib(),
                value_mib,
            )
        }
    }

    pub fn normalize_bg_workers(&self, value: u16, danger_zone_enabled: bool) -> u16 {
        if danger_zone_enabled {
            value.clamp(1, self.bg_danger_upper_workers())
        } else {
            floor_to_even_candidate(
                PERFORMANCE_BG_MIN_WORKERS,
                self.bg_normal_upper_workers(),
                value,
            )
        }
    }

    #[allow(dead_code)]
    pub fn l1_total_mib_for_danger_split(&self, value_mib: u16) -> (u16, u16) {
        split_mib_evenly(value_mib)
    }

    pub fn default_performance_settings(&self) -> PerformanceSettingsResolved {
        PerformanceSettingsResolved {
            l1_vram_cache_max_mib: self.l1_default_mib(),
            l2_ram_cache_max_mib: self.l2_default_mib(),
            background_worker_count: self.bg_default_workers() as usize,
        }
    }

    pub fn resolved_performance_settings(
        &self,
        l1_vram_cache_max_mib: u16,
        l2_ram_cache_max_mib: u16,
        background_worker_count: u16,
        danger_zone_enabled: bool,
    ) -> PerformanceSettingsResolved {
        PerformanceSettingsResolved {
            l1_vram_cache_max_mib: self
                .normalize_l1_mib(l1_vram_cache_max_mib, danger_zone_enabled),
            l2_ram_cache_max_mib: self.normalize_l2_mib(l2_ram_cache_max_mib, danger_zone_enabled),
            background_worker_count: self
                .normalize_bg_workers(background_worker_count, danger_zone_enabled)
                as usize,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_generation_stops_at_upper_bound() {
        assert_eq!(
            power_of_two_candidates(256, 4096),
            vec![256, 512, 1024, 2048, 4096]
        );
        assert_eq!(even_candidates(2, 8), vec![2, 4, 6, 8]);
    }

    #[test]
    fn floors_to_expected_candidate() {
        assert_eq!(floor_to_power_of_two_candidate(256, 4096, 3072), 2048);
        assert_eq!(floor_to_even_candidate(2, 8, 7), 6);
    }

    #[test]
    fn splits_odd_values_without_loss() {
        assert_eq!(split_mib_evenly(513), (256, 257));
    }

    #[test]
    fn defaults_follow_detected_resources() {
        let resources = PerformanceResources {
            physical_ram_bytes: 16 * 1024 * 1024 * 1024,
            dedicated_vram_bytes: None,
            logical_cpu_count: 12,
            gpu_adapter_name: None,
        };

        assert_eq!(resources.l1_default_mib(), 256);
        assert_eq!(resources.l2_default_mib(), 2048);
        assert_eq!(resources.bg_default_workers(), 2);
        assert_eq!(resources.bg_normal_upper_workers(), 6);
        assert_eq!(resources.bg_danger_upper_workers(), 9);
    }

    #[test]
    fn defaults_floor_to_normal_candidates() {
        let resources = PerformanceResources {
            physical_ram_bytes: 48 * 1024 * 1024 * 1024,
            dedicated_vram_bytes: Some(8 * 1024 * 1024 * 1024),
            logical_cpu_count: 16,
            gpu_adapter_name: None,
        };

        assert_eq!(resources.l1_default_mib(), 1024);
        assert_eq!(resources.l2_default_mib(), 4096);
        assert_eq!(resources.bg_default_workers(), 4);
        assert_eq!(resources.bg_normal_upper_workers(), 8);
        assert_eq!(resources.bg_danger_upper_workers(), 12);
    }
}
