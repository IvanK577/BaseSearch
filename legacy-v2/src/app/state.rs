use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::db::AnalyticsScope;
use crate::workers::ImportEvent;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AppTab {
    Results,
    Analytics,
}

/// Sub-tab of the Analytics view: Overview, four data categories, and the
/// cross-tab (pivot).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum AnalyticsView {
    #[default]
    Overview,
    Companies,
    Products,
    Countries,
    Prices,
    Pivot,
    Report,
    Compare,
}

impl AnalyticsView {
    pub(super) const COUNT: usize = 8;
    pub(super) const ALL: [AnalyticsView; Self::COUNT] = [
        AnalyticsView::Overview,
        AnalyticsView::Companies,
        AnalyticsView::Products,
        AnalyticsView::Countries,
        AnalyticsView::Prices,
        AnalyticsView::Pivot,
        AnalyticsView::Report,
        AnalyticsView::Compare,
    ];

    pub(super) fn index(self) -> usize {
        match self {
            AnalyticsView::Overview => 0,
            AnalyticsView::Companies => 1,
            AnalyticsView::Products => 2,
            AnalyticsView::Countries => 3,
            AnalyticsView::Prices => 4,
            AnalyticsView::Pivot => 5,
            AnalyticsView::Report => 6,
            AnalyticsView::Compare => 7,
        }
    }

    /// Section scope for the standard sub-tabs; Overview and Pivot have none.
    pub(super) fn scope(self) -> Option<AnalyticsScope> {
        match self {
            AnalyticsView::Companies => Some(AnalyticsScope::Companies),
            AnalyticsView::Products => Some(AnalyticsScope::Products),
            AnalyticsView::Countries => Some(AnalyticsScope::Countries),
            AnalyticsView::Prices => Some(AnalyticsScope::Prices),
            AnalyticsView::Overview
            | AnalyticsView::Pivot
            | AnalyticsView::Report
            | AnalyticsView::Compare => None,
        }
    }

    pub(super) fn from_scope(scope: Option<AnalyticsScope>) -> AnalyticsView {
        match scope {
            None => AnalyticsView::Overview,
            Some(AnalyticsScope::Companies) => AnalyticsView::Companies,
            Some(AnalyticsScope::Products) => AnalyticsView::Products,
            Some(AnalyticsScope::Countries) => AnalyticsView::Countries,
            Some(AnalyticsScope::Prices) => AnalyticsView::Prices,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OpKind {
    Import,
    Export,
    Clear,
    Maintenance,
}

pub(super) struct OpState {
    pub(super) kind: OpKind,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) last_event: Option<ImportEvent>,
    pub(super) export_progress: (u64, u64),
}

#[derive(Default)]
pub(super) struct StatusLine {
    pub(super) text: String,
    pub(super) is_error: bool,
}

pub(super) fn invalidate_underpricing_generation(generation: &mut u64) {
    *generation = generation.wrapping_add(1);
}
