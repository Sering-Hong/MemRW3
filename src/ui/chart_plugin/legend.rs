use eframe::egui::Color32;
use egui_plot::PlotPoint;
use std::cell::RefCell;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct HistoryKey {
    revision: u64,
    len: usize,
    first: Option<[f64; 2]>,
    last: Option<[f64; 2]>,
}
type Bounds = (f64, f64, f64, f64);
type BoundsCache = Option<(HistoryKey, Option<Bounds>)>;
type PlotCache = Option<((HistoryKey, f64, f64, usize), Vec<PlotPoint>)>;
fn history_key(history: &VecDeque<PlotPoint>, revision: u64) -> HistoryKey {
    HistoryKey {
        revision,
        len: history.len(),
        first: history.front().map(|p| [p.x, p.y]),
        last: history.back().map(|p| [p.x, p.y]),
    }
}

pub struct ChartLegend {
    pub fft_cache: super::fft::FftCache,
    pub variable_id: usize,
    pub curve_name: String,
    pub variable_path: String,
    pub comment: String,
    pub color: Color32,
    pub visible: bool,
    pub buffer_size: usize,
    pub data_history: VecDeque<PlotPoint>,
    pub plot_bridge: Option<[PlotPoint; 2]>,
    revision: u64,
    bounds_cache: RefCell<BoundsCache>,
    plot_cache: RefCell<PlotCache>,
}

impl ChartLegend {
    pub fn new(variable_id: usize, curve_name: String) -> Self {
        Self {
            fft_cache: super::fft::FftCache::default(),
            variable_id,
            variable_path: curve_name.clone(),
            comment: String::new(),
            curve_name,
            color: default_color(variable_id),
            visible: true,
            buffer_size: 0, // Zero means retain the complete acquisition history.
            data_history: VecDeque::with_capacity(5000),
            plot_bridge: None,
            revision: 0,
            bounds_cache: RefCell::new(None),
            plot_cache: RefCell::new(None),
        }
    }

    /// Make room for an incoming batch and return how many oldest incoming
    /// samples can be skipped because they would be evicted immediately.
    pub fn prepare_batch(&mut self, incoming_len: usize) -> usize {
        if self.buffer_size == 0 {
            return 0;
        }

        if incoming_len >= self.buffer_size {
            self.data_history.clear();
            return incoming_len - self.buffer_size;
        }

        let overflow = self
            .data_history
            .len()
            .saturating_add(incoming_len)
            .saturating_sub(self.buffer_size);
        if overflow > 0 {
            drop(self.data_history.drain(..overflow));
        }
        0
    }

    pub fn push_prepared(&mut self, time: f64, value: f64) {
        debug_assert!(self.buffer_size == 0 || self.data_history.len() < self.buffer_size);
        let old_key = history_key(&self.data_history, self.revision);
        self.data_history.push_back(PlotPoint::new(time, value));
        self.revision = self.revision.wrapping_add(1);
        let key = history_key(&self.data_history, self.revision);
        let mut cache = self.bounds_cache.borrow_mut();
        if let Some((cached_key, bounds)) = cache.as_mut() {
            if *cached_key == old_key && time.is_finite() && value.is_finite() {
                *bounds = Some(match *bounds {
                    Some((a, b, c, d)) => (a.min(time), b.max(time), c.min(value), d.max(value)),
                    None => (time, time, value, value),
                });
                *cached_key = key;
            } else {
                *cache = None;
            }
        }
    }

    pub fn cached_bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let key = history_key(&self.data_history, self.revision);
        let mut cache = self.bounds_cache.borrow_mut();
        if let Some((cached_key, bounds)) = *cache {
            if cached_key == key {
                return bounds;
            }
        }
        let mut bounds: Option<Bounds> = None;
        for point in &self.data_history {
            if !point.x.is_finite() || !point.y.is_finite() {
                continue;
            }
            bounds = Some(match bounds {
                Some((a, b, c, d)) => (
                    a.min(point.x),
                    b.max(point.x),
                    c.min(point.y),
                    d.max(point.y),
                ),
                None => (point.x, point.x, point.y, point.y),
            });
        }
        *cache = Some((key, bounds));
        bounds
    }
    pub fn cached_plot_points(&self, x_min: f64, x_max: f64, pixels: usize) -> Vec<PlotPoint> {
        let key = (
            history_key(&self.data_history, self.revision),
            x_min,
            x_max,
            pixels,
        );
        let mut cache = self.plot_cache.borrow_mut();
        if let Some((old_key, points)) = cache.as_ref() {
            if *old_key == key {
                return points.clone();
            }
        }
        let points = super::panel::history_plot_points(&self.data_history, x_min, x_max, pixels);
        *cache = Some((key, points.clone()));
        points
    }

    pub fn refresh_plot_bridge(&mut self) {
        let (first, second) = self.data_history.as_slices();
        self.plot_bridge = first
            .last()
            .copied()
            .zip(second.first().copied())
            .map(|(left, right)| [left, right]);
    }
}

const PRESET_COLORS: [Color32; 12] = [
    Color32::from_rgb(66, 133, 244),
    Color32::from_rgb(219, 68, 55),
    Color32::from_rgb(244, 180, 0),
    Color32::from_rgb(15, 157, 88),
    Color32::from_rgb(171, 71, 188),
    Color32::from_rgb(0, 172, 193),
    Color32::from_rgb(255, 112, 67),
    Color32::from_rgb(63, 81, 181),
    Color32::from_rgb(139, 195, 74),
    Color32::from_rgb(255, 87, 34),
    Color32::from_rgb(158, 158, 158),
    Color32::from_rgb(121, 85, 72),
];
pub(super) fn default_color(index: usize) -> Color32 {
    PRESET_COLORS[index % PRESET_COLORS.len()]
}
pub const fn preset_colors() -> &'static [Color32; 12] {
    &PRESET_COLORS
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use egui_plot::PlotPoint;

    use super::ChartLegend;

    #[test]
    fn retains_a_full_capture_beyond_the_old_limit() {
        let mut legend = ChartLegend::new(0, "full capture".to_owned());
        for batch in 0..120 {
            assert_eq!(legend.prepare_batch(1000), 0);
            for offset in 0..1000 {
                let time = (batch * 1000 + offset) as f64 / 1000.0;
                legend.push_prepared(time, time);
            }
        }
        assert_eq!(legend.data_history.len(), 120_000);
        assert_eq!(legend.data_history.front().unwrap().x, 0.0);
        assert_eq!(legend.data_history.back().unwrap().x, 119.999);
    }

    #[test]
    fn prepares_space_for_a_batch_once() {
        let mut legend = ChartLegend::new(0, "test".to_owned());
        legend.buffer_size = 4;
        legend.data_history.extend(
            [(1.0, 1.0), (2.0, 2.0), (3.0, 3.0)].map(|(x, y)| egui_plot::PlotPoint::new(x, y)),
        );

        assert_eq!(legend.prepare_batch(2), 0);
        legend.push_prepared(4.0, 4.0);
        legend.push_prepared(5.0, 5.0);

        assert_eq!(
            legend
                .data_history
                .iter()
                .map(|point| (point.x, point.y))
                .collect::<Vec<_>>(),
            vec![(2.0, 2.0), (3.0, 3.0), (4.0, 4.0), (5.0, 5.0)]
        );
    }

    #[test]
    fn skips_samples_that_cannot_fit() {
        let mut legend = ChartLegend::new(0, "test".to_owned());
        legend.buffer_size = 2;
        legend
            .data_history
            .push_back(egui_plot::PlotPoint::new(1.0, 1.0));

        assert_eq!(legend.prepare_batch(4), 2);
        legend.push_prepared(4.0, 4.0);
        legend.push_prepared(5.0, 5.0);

        assert_eq!(
            legend
                .data_history
                .iter()
                .map(|point| (point.x, point.y))
                .collect::<Vec<_>>(),
            vec![(4.0, 4.0), (5.0, 5.0)]
        );
    }

    #[test]
    fn bridges_wrapped_plot_slices() {
        let mut legend = ChartLegend::new(0, "test".to_owned());
        legend.buffer_size = 4;
        legend.data_history = VecDeque::with_capacity(4);
        for value in 0..4 {
            legend.data_history.push_back(PlotPoint::new(value, value));
        }
        legend.prepare_batch(1);
        legend.push_prepared(4.0, 4.0);

        let (_, wrapped) = legend.data_history.as_slices();
        assert!(!wrapped.is_empty());
        legend.refresh_plot_bridge();

        assert_eq!(
            legend.plot_bridge,
            Some([PlotPoint::new(3.0, 3.0), PlotPoint::new(4.0, 4.0)])
        );
    }
}

#[cfg(test)]
mod enhanced_cache_tests {
    use super::*;
    #[test]
    fn caches_invalidate_on_append_clear_and_eviction() {
        let mut line = ChartLegend::new(0, "line".into());
        line.push_prepared(0.0, 1.0);
        line.push_prepared(1.0, 9.0);
        assert_eq!(line.cached_bounds(), Some((0.0, 1.0, 1.0, 9.0)));
        let _ = line.cached_plot_points(0.0, 10.0, 800);
        line.push_prepared(2.0, -5.0);
        assert_eq!(line.cached_bounds(), Some((0.0, 2.0, -5.0, 9.0)));
        assert_eq!(line.cached_plot_points(0.0, 10.0, 800).len(), 3);
        line.buffer_size = 1;
        line.prepare_batch(1);
        line.push_prepared(3.0, 2.0);
        assert_eq!(line.cached_bounds(), Some((3.0, 3.0, 2.0, 2.0)));
        line.data_history.clear();
        assert_eq!(line.cached_bounds(), None);
        assert!(line.cached_plot_points(0.0, 10.0, 800).is_empty());
    }
    #[test]
    fn three_long_curves_keep_peaks_and_warm_cache_is_reused() {
        let start = std::time::Instant::now();
        let mut lines: Vec<_> = (0..3)
            .map(|i| ChartLegend::new(i, format!("curve_{i}")))
            .collect();
        for line in &mut lines {
            for i in 0..120_000 {
                line.push_prepared(
                    i as f64 * 0.001,
                    if i == 60_001 {
                        999.0
                    } else {
                        (i as f64 * 0.01).sin()
                    },
                );
            }
        }
        for line in &lines {
            assert_eq!(line.data_history.len(), 120_000);
            assert_eq!(line.cached_bounds().unwrap().3, 999.0);
            let points = line.cached_plot_points(0.0, 120.0, 1200);
            assert!(points.iter().any(|p| p.y == 999.0));
            assert_eq!(
                points.len(),
                line.cached_plot_points(0.0, 120.0, 1200).len()
            );
        }
        let warm = std::time::Instant::now();
        for _ in 0..100 {
            for line in &lines {
                let _ = line.cached_bounds();
                let _ = line.cached_plot_points(0.0, 120.0, 1200);
            }
        }
        eprintln!(
            "display-cache: prepare {:?}, 100 warm frames {:?}",
            warm.duration_since(start),
            warm.elapsed()
        );
    }
}
