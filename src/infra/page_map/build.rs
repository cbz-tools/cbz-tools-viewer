use crate::domain::page_map::{BookPageMap, PageDescriptor, SourceRevision};
use crate::infra::archive::page_map::{
    ZipPageMapFastStatus, ZipPageMapIssueReason, ZipPageMapSlowReason,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageMapBuildStatus {
    Ready,
    RequiresComplete(ZipPageMapSlowReason),
    Failed(ZipPageMapIssueReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZipFastPageMapAssemblyResult {
    pub status: PageMapBuildStatus,
    pub page_map: Option<BookPageMap>,
}

pub type FastBuildOutcome = ZipFastPageMapAssemblyResult;

pub fn assemble_zip_fast_page_map(
    revision: SourceRevision,
    page_count: u32,
    fast_lane_status: ZipPageMapFastStatus,
    fast_lane_pages: Vec<PageDescriptor>,
) -> ZipFastPageMapAssemblyResult {
    let (status, pages) = match fast_lane_status {
        ZipPageMapFastStatus::Ready => {
            if fast_lane_pages.len() != page_count as usize {
                (
                    PageMapBuildStatus::Failed(ZipPageMapIssueReason::ZipStructure),
                    None,
                )
            } else {
                (PageMapBuildStatus::Ready, Some(fast_lane_pages))
            }
        }
        ZipPageMapFastStatus::SlowRequired(reason) => {
            (PageMapBuildStatus::RequiresComplete(reason), None)
        }
        ZipPageMapFastStatus::Failed(reason) => (PageMapBuildStatus::Failed(reason), None),
    };

    ZipFastPageMapAssemblyResult {
        status,
        page_map: pages.map(|pages| BookPageMap::new(revision, pages)),
    }
}
