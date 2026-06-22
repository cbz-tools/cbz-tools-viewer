use std::convert::TryFrom;

use crate::domain::page_map::BookPageMap;

/// AUTO ルールで縦長判定する。
fn is_portrait_page(orig_w: u32, orig_h: u32) -> Option<bool> {
    (orig_w > 0).then_some(orig_h as f32 / orig_w as f32 >= 1.1)
}

/// 論理ページのメタ情報から作る不変の AUTO 表示単位。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AutoDisplayUnit {
    first_page: u32,
    second_page: Option<u32>,
}

impl AutoDisplayUnit {
    fn new(first_page: u32, second_page: Option<u32>) -> Self {
        Self {
            first_page,
            second_page,
        }
    }

    /// この表示単位の論理アンカー page を返す。
    pub(crate) fn anchor_page(&self) -> u32 {
        self.first_page
    }

    /// この表示単位に含まれる論理 page を返す。
    pub(crate) fn pages(&self) -> (u32, Option<u32>) {
        (self.first_page, self.second_page)
    }
}

/// 論理 page で引ける不変の AUTO 表示計画。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AutoSpreadPlan {
    units: Vec<AutoDisplayUnit>,
    page_to_unit: Vec<usize>,
}

impl AutoSpreadPlan {
    /// `logical_page` を含む表示単位を返す。
    pub(crate) fn unit_for_logical_page(&self, logical_page: u32) -> Option<&AutoDisplayUnit> {
        self.unit_index_for_logical_page(logical_page)
            .and_then(|unit_index| self.units.get(unit_index))
    }

    /// `logical_page` を含む単位の論理アンカー page を返す。
    pub(crate) fn anchor_for_logical_page(&self, logical_page: u32) -> Option<u32> {
        self.unit_for_logical_page(logical_page)
            .map(AutoDisplayUnit::anchor_page)
    }

    /// `unit_index` の論理 page を返す。
    ///
    /// `unit_index` は内部の 0-based 計画 index であり、論理 page ではない。
    pub(crate) fn pages_for_unit_index(&self, unit_index: usize) -> Option<(u32, Option<u32>)> {
        self.units.get(unit_index).map(AutoDisplayUnit::pages)
    }

    /// `logical_page` を含む単位の論理 page を返す。
    pub(crate) fn pages_for_logical_page(&self, logical_page: u32) -> Option<(u32, Option<u32>)> {
        self.unit_index_for_logical_page(logical_page)
            .and_then(|unit_index| self.pages_for_unit_index(unit_index))
    }

    /// `logical_page_or_anchor` の次単位の論理アンカー page を返す。
    pub(crate) fn next_anchor(&self, logical_page_or_anchor: u32) -> Option<u32> {
        let unit_index = self.unit_index_for_logical_page(logical_page_or_anchor)?;
        self.units
            .get(unit_index.checked_add(1)?)
            .map(AutoDisplayUnit::anchor_page)
    }

    /// `logical_page_or_anchor` の前単位の論理アンカー page を返す。
    pub(crate) fn previous_anchor(&self, logical_page_or_anchor: u32) -> Option<u32> {
        let unit_index = self.unit_index_for_logical_page(logical_page_or_anchor)?;
        self.units
            .get(unit_index.checked_sub(1)?)
            .map(AutoDisplayUnit::anchor_page)
    }

    fn unit_index_for_logical_page(&self, logical_page: u32) -> Option<usize> {
        let page_index = usize::try_from(logical_page).ok()?;
        let unit_index = *self.page_to_unit.get(page_index)?;
        self.units.get(unit_index)?;
        Some(unit_index)
    }
}

/// 論理 page のメタ情報から不変の AUTO 表示計画を作る。
pub(crate) fn build_auto_spread_plan(
    page_map: &BookPageMap,
    cover_blank: bool,
) -> Option<AutoSpreadPlan> {
    let page_count = page_map.page_count();
    let mut units = Vec::with_capacity(page_count);
    let mut page_to_unit = Vec::with_capacity(page_count);

    if page_count == 0 {
        return Some(AutoSpreadPlan {
            units,
            page_to_unit,
        });
    }

    let mut page = 0usize;
    while page < page_count {
        let current = page_map.get(page)?;
        let current_is_portrait = is_portrait_page(current.width, current.height)?;
        let unit_index = units.len();
        let first_page = u32::try_from(page).ok()?;

        if cover_blank && page == 0 {
            units.push(AutoDisplayUnit::new(first_page, None));
            page_to_unit.push(unit_index);
            page = page.saturating_add(1);
            continue;
        }

        let next = page.checked_add(1);
        let can_spread = next.and_then(|next_page| {
            let next = page_map.get(next_page)?;
            let next_is_portrait = is_portrait_page(next.width, next.height)?;
            Some(current_is_portrait && next_is_portrait)
        }) == Some(true);

        if can_spread {
            let second_page = u32::try_from(page + 1).ok()?;
            units.push(AutoDisplayUnit::new(first_page, Some(second_page)));
            page_to_unit.push(unit_index);
            page_to_unit.push(unit_index);
            page = page.saturating_add(2);
        } else {
            units.push(AutoDisplayUnit::new(first_page, None));
            page_to_unit.push(unit_index);
            page = page.saturating_add(1);
        }
    }

    Some(AutoSpreadPlan {
        units,
        page_to_unit,
    })
}
