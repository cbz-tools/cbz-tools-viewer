use crate::domain::{app_settings::ViewerQuality, archive::BookId, archive_settings::SpreadMode};

const RENDER_SIGNATURE_SUITABILITY_NUMERATOR: u32 = 105;
const RENDER_SIGNATURE_SUITABILITY_DENOMINATOR: u32 = 100;

/// BG / interactive がどの decode 条件で生成されたかを表す署名。
/// 表示成立条件は別型に分離する。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct RenderSignature {
    pub(super) quality: ViewerQuality,
    /// decode request の基準幅。生成実寸とは限らない。
    pub(super) target_w: u32,
    /// decode request の基準高さ。生成実寸とは限らない。
    pub(super) target_h: u32,
    /// GPU 上限は decode 条件として保持する。
    pub(super) max_tex_side: u32,
}

impl RenderSignature {
    pub(super) fn from_decode_request(
        quality: ViewerQuality,
        target_w: u32,
        target_h: u32,
        max_tex_side: u32,
    ) -> Self {
        Self {
            quality,
            target_w,
            target_h,
            max_tex_side,
        }
    }

    pub(super) fn is_suitable_for(self, requirement: DisplayRequirement) -> bool {
        self.quality == requirement.quality
            && self.max_tex_side == requirement.max_tex_side
            && self
                .target_w
                .saturating_mul(RENDER_SIGNATURE_SUITABILITY_NUMERATOR)
                >= requirement
                    .required_w
                    .saturating_mul(RENDER_SIGNATURE_SUITABILITY_DENOMINATOR)
            && self
                .target_h
                .saturating_mul(RENDER_SIGNATURE_SUITABILITY_NUMERATOR)
                >= requirement
                    .required_h
                    .saturating_mul(RENDER_SIGNATURE_SUITABILITY_DENOMINATOR)
    }

    #[allow(dead_code)]
    pub(super) fn mismatch_reason(self, requirement: DisplayRequirement) -> Option<&'static str> {
        if self.quality != requirement.quality {
            return Some("quality mismatch");
        }
        if self.max_tex_side != requirement.max_tex_side {
            return Some("max_tex_side mismatch");
        }
        if self
            .target_w
            .saturating_mul(RENDER_SIGNATURE_SUITABILITY_NUMERATOR)
            < requirement
                .required_w
                .saturating_mul(RENDER_SIGNATURE_SUITABILITY_DENOMINATOR)
            || self
                .target_h
                .saturating_mul(RENDER_SIGNATURE_SUITABILITY_NUMERATOR)
                < requirement
                    .required_h
                    .saturating_mul(RENDER_SIGNATURE_SUITABILITY_DENOMINATOR)
        {
            return Some("insufficient resolution");
        }
        None
    }
}

/// 保存 key の完全一致ではなく、`RenderSignature::is_suitable_for()` の 5% tolerance を使って
/// History / Warm / Planner の同一 page 内候補を決める。
pub(super) fn page_render_signature_rank(
    candidate_page: u32,
    candidate_signature: RenderSignature,
    required_page: u32,
    requirement: DisplayRequirement,
    bytes: usize,
) -> Option<(u64, u32, u32, usize)> {
    if candidate_page != required_page {
        return None;
    }
    if !candidate_signature.is_suitable_for(requirement) {
        return None;
    }
    let score = candidate_signature
        .target_w
        .abs_diff(requirement.required_w) as u64
        + candidate_signature
            .target_h
            .abs_diff(requirement.required_h) as u64;
    Some((
        score,
        candidate_signature.target_w,
        candidate_signature.target_h,
        bytes,
    ))
}

/// 表示成立に必要な最低条件。`RenderSignature` の要求寸法と比較する。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct DisplayRequirement {
    pub(super) quality: ViewerQuality,
    /// 表示成立に必要な最小幅。
    pub(super) required_w: u32,
    /// 表示成立に必要な最小高さ。
    pub(super) required_h: u32,
    /// 表示条件に使う GPU 上限。
    pub(super) max_tex_side: u32,
}

impl DisplayRequirement {
    pub(super) fn from_display_request(
        quality: ViewerQuality,
        required_w: u32,
        required_h: u32,
        max_tex_side: u32,
    ) -> Self {
        Self {
            quality,
            required_w,
            required_h,
            max_tex_side,
        }
    }
}

/// Working-set のナビゲーション起点。
/// フィールド値は物理 page index で持ち、spread / unit index は使わない。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum WorkingSetAnchorPage {
    /// ナビゲーションで直接選ばれた物理 page。
    Single { requested_page: u32 },
    /// forced-spread のナビゲーション基準物理 page。
    Spread { navigation_page: u32 },
    /// AUTO のナビゲーション基準物理 page。
    Auto { navigation_page: u32 },
}

impl WorkingSetAnchorPage {
    /// この anchor が表す物理 page index を返す。
    pub(super) fn navigation_page(self) -> u32 {
        match self {
            Self::Single { requested_page } => requested_page,
            Self::Spread { navigation_page } => navigation_page,
            Self::Auto { navigation_page } => navigation_page,
        }
    }
}

/// 進行方向を厚く、逆方向を少し残すための評価方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Direction {
    Forward,
    Backward,
}

impl Direction {
    pub(super) fn from_nav_delta(delta: i32) -> Option<Self> {
        match delta.cmp(&0) {
            std::cmp::Ordering::Greater => Some(Self::Forward),
            std::cmp::Ordering::Less => Some(Self::Backward),
            std::cmp::Ordering::Equal => None,
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Backward => "backward",
        }
    }

    pub(super) fn signed_offset(self, step_index: usize) -> i64 {
        match self {
            Self::Forward => match step_index {
                0 => 1,
                n if n % 2 == 1 => (n as i64 / 2) + 2,
                n => -(n as i64 / 2),
            },
            Self::Backward => match step_index {
                0 => -1,
                n if n % 2 == 1 => -((n as i64 / 2) + 2),
                n => n as i64 / 2,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct PageRenderSignatureKey {
    pub(super) page: u32,
    pub(super) render_signature: RenderSignature,
}

/// GPU 層の共通メタ情報。History / Warmup / Planner が同じ構造を参照する。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct GpuTextureEntrySnapshot {
    pub(super) key: PageRenderSignatureKey,
    pub(super) page: u32,
    pub(super) bytes: usize,
}

/// 巨大 page を BG へ再投入し続けるループを防ぐための負キャッシュ。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BgAdmissionState {
    Admissible,
    TooLargeForBgRgba,
    InsertDidNotSurvive,
}

/// BG 結果を受理するための明示的な render 条件。
#[derive(Clone, Debug)]
pub(super) struct BgRenderContext {
    pub(super) book_id: BookId,
    pub(super) quality: ViewerQuality,
    pub(super) spread_setting: SpreadMode,
    pub(super) spread_mode: bool,
    pub(super) cover_blank: bool,
    pub(super) full_equivalent_area_w: u32,
    pub(super) full_equivalent_area_h: u32,
    pub(super) max_tex_side: u32,
}

/// Working Set は論理的に欲しい page 集合であり、実際に RGBA へ保持できた page 集合ではない。
/// page 単位で束ね、BG 先読みの投入順と採否を分離する。
#[derive(Clone, Copy, Debug)]
pub(super) struct WorkingSetPage {
    pub(super) page: u32,
}

/// page 単位の Working Set を保持し、BG FIFO と eviction の基準を共通化する。
/// Worker 数や FIFO 容量はここでは扱わない。
#[derive(Clone, Debug)]
pub(super) struct WorkingSetPlan {
    pages: Vec<WorkingSetPage>,
}

impl WorkingSetPlan {
    pub(super) fn new(_anchor_page: WorkingSetAnchorPage, _direction: Direction) -> Self {
        Self { pages: Vec::new() }
    }

    pub(super) fn push(&mut self, page: WorkingSetPage) {
        self.pages.push(page);
    }

    pub(super) fn pages(&self) -> &[WorkingSetPage] {
        &self.pages
    }

    pub(super) fn page_count(&self) -> usize {
        self.pages.len()
    }
}

/// BG decode の結果を render_signature 単位で追跡し、古い結果の混入を防ぐ。
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(super) struct BgInflightEntry {
    pub(super) request_id: u64,
    pub(super) page: u32,
    pub(super) render_signature: RenderSignature,
    pub(super) render_context: BgRenderContext,
    pub(super) working_set_anchor_page: WorkingSetAnchorPage,
    pub(super) source_view: Option<u32>,
}

impl BgInflightEntry {
    pub(super) fn key(&self) -> PageRenderSignatureKey {
        PageRenderSignatureKey {
            page: self.page,
            render_signature: self.render_signature,
        }
    }
}
