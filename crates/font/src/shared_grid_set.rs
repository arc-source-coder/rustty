use std::hash::Hash;
use std::sync::Mutex;

use rapidhash::RapidHashMap;

#[cfg(target_os = "windows")]
use crate::backend::dwrite::fallback::FontFallbackContext;
#[cfg(target_os = "windows")]
use crate::backend::dwrite::variation::StyleVariationRequest;
#[cfg(target_os = "windows")]
use crate::collection::Collection;
#[cfg(target_os = "windows")]
use crate::resolver::CodepointResolver;
#[cfg(target_os = "windows")]
use crate::shared_grid::GridMetrics;
use crate::shared_grid::SharedGrid;
#[cfg(target_os = "windows")]
use crate::types::{FontAxisSpec, Style};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::DirectWrite::IDWriteFactory6;
#[cfg(target_os = "windows")]
use windows_core::Interface;

/// Ghostty-style shared-grid registry keyed by derived font configuration.
///
/// This struct is intentionally explicit about ref/deref so renderer-side
/// ownership can mirror Ghostty's surface lifecycle.
pub struct SharedGridSet<K>
where
    K: Eq + Hash + Clone,
{
    inner: Mutex<RapidHashMap<K, ReffedGrid>>,
}

struct ReffedGrid {
    grid: Box<SharedGrid>,
    refs: u32,
}

/// Stable pointer to a SharedGrid owned by SharedGridSet.
///
/// Mirrors Ghostty's `*SharedGrid` return from `SharedGridSet.ref`.
pub type SharedGridPtr = *const SharedGrid;

impl<K> SharedGridSet<K>
where
    K: Eq + Hash + Clone,
{
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RapidHashMap::default()),
        }
    }

    pub fn count(&self) -> usize {
        self.inner.lock().expect("shared grid set poisoned").len()
    }

    /// Increment ref for `key`, creating a new grid with `init` when absent.
    pub fn ref_or_insert_with<F>(&self, key: K, init: F) -> SharedGridPtr
    where
        F: FnOnce() -> SharedGrid,
    {
        self.try_ref_or_insert_with(key, || Ok::<_, core::convert::Infallible>(init()))
            .expect("infallible init")
    }

    /// Increment ref for `key`, creating a new grid with a fallible `init` when absent.
    // TODO: Do something about the generics
    pub fn try_ref_or_insert_with<F, E>(
        &self,
        key: K,
        init: F,
    ) -> std::result::Result<SharedGridPtr, E>
    where
        F: FnOnce() -> std::result::Result<SharedGrid, E>,
    {
        {
            let mut inner = self.inner.lock().expect("shared grid set poisoned");
            if let Some(existing) = inner.get_mut(&key) {
                existing.refs += 1;
                return Ok(existing.grid.as_ref() as *const SharedGrid);
            }
        }

        let grid = Box::new(init()?);
        let mut inner = self.inner.lock().expect("shared grid set poisoned");
        if let Some(existing) = inner.get_mut(&key) {
            existing.refs += 1;
            return Ok(existing.grid.as_ref() as *const SharedGrid);
        }
        let ptr = grid.as_ref() as *const SharedGrid;
        inner.insert(key, ReffedGrid { grid, refs: 1 });
        Ok(ptr)
    }

    /// Decrement ref for `key`. When it reaches zero, removes the grid.
    pub fn deref(&self, key: &K) {
        let mut inner = self.inner.lock().expect("shared grid set poisoned");
        let Some(entry) = inner.get_mut(key) else {
            return;
        };
        if entry.refs > 1 {
            entry.refs -= 1;
            return;
        }
        inner.remove(key);
    }
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DWriteStyleKey {
    pub family: String,
    pub axes: Vec<(u32, i64)>,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DWriteFallbackKey {
    pub base_family_ptr: u64,
    pub base_collection_ptr: u64,
    pub fallback_ptr: u64,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DWriteGridKey {
    pub locale: String,
    pub metrics: GridMetrics,
    pub max_atlas_size: u32,
    pub styles: [DWriteStyleKey; Style::COUNT],
    pub fallback: Option<DWriteFallbackKey>,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug)]
pub struct DWriteStyleConfig {
    pub family: String,
    pub axes: FontAxisSpec,
}

#[cfg(target_os = "windows")]
#[derive(Clone)]
pub struct DWriteGridConfig {
    pub locale: String,
    pub styles: [DWriteStyleConfig; Style::COUNT],
    pub metrics: GridMetrics,
    pub max_atlas_size: u32,
    pub fallback: Option<FontFallbackContext>,
}

#[cfg(target_os = "windows")]
impl DWriteGridConfig {
    pub fn with_single_family(family: impl Into<String>, locale: impl Into<String>) -> Self {
        let family = family.into();
        Self {
            locale: locale.into(),
            styles: std::array::from_fn(|_| DWriteStyleConfig {
                family: family.clone(),
                axes: FontAxisSpec::default(),
            }),
            metrics: GridMetrics::default(),
            max_atlas_size: 0,
            fallback: None,
        }
    }
}

#[cfg(target_os = "windows")]
impl DWriteGridKey {
    /// Ghostty reference:
    /// `SharedGridSet.Key` and `discovery.Descriptor.hash` include family and
    /// variation identity at grid lifecycle boundaries.
    pub fn from_config(config: &DWriteGridConfig) -> Self {
        Self {
            locale: config.locale.clone(),
            metrics: config.metrics,
            max_atlas_size: config.max_atlas_size,
            styles: std::array::from_fn(|i| {
                let style = &config.styles[i];
                DWriteStyleKey {
                    family: style.family.clone(),
                    // Match Ghostty descriptor hashing spirit: axis tag + int value.
                    axes: style
                        .axes
                        .values
                        .iter()
                        .map(|v| (v.axisTag.0, v.value as i64))
                        .collect(),
                }
            }),
            fallback: config.fallback.as_ref().map(|v| DWriteFallbackKey {
                // We key by COM identity/pointer because fallback internals are
                // COM objects and do not have value-based Rust equality.
                base_family_ptr: v.base_family.as_ptr() as usize as u64,
                base_collection_ptr: v.base_collection.as_raw().addr() as u64,
                fallback_ptr: v.fallback.as_raw().addr() as u64,
            }),
        }
    }
}

#[cfg(target_os = "windows")]
impl SharedGridSet<DWriteGridKey> {
    /// Resolve/configure a shared grid from a Ghostty-like config key and return
    /// `(key, grid)`. The key must later be passed to `deref`.
    ///
    /// Ghostty reference:
    /// `SharedGridSet.ref` initializes a grid from config-derived key data.
    pub fn ref_dwrite(
        &self,
        factory: &IDWriteFactory6,
        config: &DWriteGridConfig,
    ) -> anyhow::Result<(DWriteGridKey, SharedGridPtr)> {
        let key = DWriteGridKey::from_config(config);
        let grid =
            self.try_ref_or_insert_with(key.clone(), move || -> anyhow::Result<SharedGrid> {
                let grid = SharedGrid::with_atlas_max_size(
                    CodepointResolver::new(Collection::new()),
                    config.metrics,
                    config.max_atlas_size,
                );

                let requests: [StyleVariationRequest<'_>; Style::COUNT] =
                    std::array::from_fn(|i| StyleVariationRequest {
                        family: &config.styles[i].family,
                        axes: config.styles[i].axes.clone(),
                    });
                grid.configure_dwrite_primary_faces(factory, &requests)?;
                if let Some(fallback) = config.fallback.clone() {
                    grid.set_dwrite_fallback(fallback, &config.locale);
                }
                Ok(grid)
            })?;
        Ok((key, grid))
    }
}

impl<K> Default for SharedGridSet<K>
where
    K: Eq + Hash + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::collection::Collection;
    use crate::shared_grid::GridMetrics;

    #[test]
    fn same_key_reuses_grid_and_refcounts() {
        let set = SharedGridSet::<u64>::new();
        let g1 = set.ref_or_insert_with(1, || {
            SharedGrid::with_collection(Collection::new(), GridMetrics::default())
        });
        let g2 = set.ref_or_insert_with(1, || {
            SharedGrid::with_collection(Collection::new(), GridMetrics::default())
        });
        assert_eq!(g1, g2);
        assert_eq!(set.count(), 1);
        set.deref(&1);
        assert_eq!(set.count(), 1);
        set.deref(&1);
        assert_eq!(set.count(), 0);
    }
}
