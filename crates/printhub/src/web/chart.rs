//! Stacked bar charts drawn as inline SVG. The geometry is computed here, so the template only
//! places elements.

const WIDTH: f64 = 960.0;
const HEIGHT: f64 = 260.0;
const LEFT: f64 = 64.0;
const RIGHT: f64 = 12.0;
const TOP: f64 = 12.0;
/// Room under the baseline for the bar labels.
const BOTTOM: f64 = 30.0;
const MAX_BAR: f64 = 24.0;
/// The surface-coloured gap between stacked segments.
const GAP: f64 = 2.0;
const RADIUS: f64 = 4.0;
/// Labelling more bars than this makes the labels collide.
const MAX_LABELS: usize = 12;

/// Categorical colour classes, defined in `app.css`. Their order was checked for colour-blind
/// separation of neighbours, so series take them in this order.
pub const SLOTS: [&str; 8] = [
    "series-1", "series-2", "series-3", "series-4", "series-5", "series-6", "series-7", "series-8",
];
/// For whatever is folded together once the slots run out.
pub const OTHER_SLOT: &str = "series-other";

pub struct Series {
    pub name: String,
    pub slot: &'static str,
}

pub struct BarInput {
    /// Under the bar, when there is room for it.
    pub short_label: String,
    /// In tooltips and the table view.
    pub label: String,
    /// One per series, in series order.
    pub values: Vec<f64>,
}

pub struct BarChart {
    pub view_box: String,
    pub description: String,
    pub grid: Vec<GridLine>,
    pub bars: Vec<Bar>,
    pub plot_left: String,
    pub plot_right: String,
    pub tick_x: String,
    pub label_y: String,
    /// Empty for a single series, which the heading names.
    pub legend: Vec<LegendItem>,
    pub table_head: Vec<String>,
    pub table_rows: Vec<Vec<String>>,
}

pub struct GridLine {
    pub y: String,
    pub text_y: String,
    pub label: String,
}

pub struct Bar {
    pub center: String,
    pub label: Option<String>,
    pub segments: Vec<Segment>,
}

pub struct Segment {
    pub path: String,
    pub slot: &'static str,
    pub title: String,
}

pub struct LegendItem {
    pub name: String,
    pub slot: &'static str,
}

/// `None` when every value is zero: an empty plot says less than a sentence.
pub fn stacked(
    description: String,
    series: Vec<Series>,
    bars: Vec<BarInput>,
    format: impl Fn(f64) -> String,
) -> Option<BarChart> {
    let total = |bar: &BarInput| bar.values.iter().filter(|v| **v > 0.0).sum::<f64>();
    let max = bars.iter().map(total).fold(0.0, f64::max);
    if max <= 0.0 {
        return None;
    }
    let (step, steps) = ticks(max);
    let top_value = step * steps as f64;
    let baseline = HEIGHT - BOTTOM;
    let scale = (baseline - TOP) / top_value;

    let grid = (0..=steps)
        .map(|i| {
            let value = step * i as f64;
            let y = baseline - value * scale;
            GridLine {
                y: coord(y),
                text_y: coord(y + 4.0),
                label: format(value),
            }
        })
        .collect();

    let shown: Vec<bool> = series
        .iter()
        .enumerate()
        .map(|(index, _)| {
            bars.iter()
                .any(|bar| bar.values.get(index).is_some_and(|v| *v > 0.0))
        })
        .collect();

    let band = (WIDTH - LEFT - RIGHT) / bars.len() as f64;
    let width = MAX_BAR.min(band * 0.7);
    let label_every = bars.len().div_ceil(MAX_LABELS);
    let drawn = bars
        .iter()
        .enumerate()
        .map(|(index, bar)| {
            let x = LEFT + band * index as f64 + (band - width) / 2.0;
            let filled: Vec<(usize, f64)> = bar
                .values
                .iter()
                .copied()
                .enumerate()
                .filter(|(_, value)| *value > 0.0)
                .collect();
            let mut below = 0.0;
            let mut segments = Vec::new();
            for (position, (series_index, value)) in filled.iter().enumerate() {
                let mut bottom = baseline - below * scale;
                below += value;
                if position > 0 {
                    bottom -= GAP;
                }
                let top = baseline - below * scale;
                if bottom - top < 0.5 {
                    continue;
                }
                let rounded = if position + 1 == filled.len() {
                    RADIUS
                } else {
                    0.0
                };
                let name = &series[*series_index].name;
                segments.push(Segment {
                    path: bar_path(x, top, width, bottom - top, rounded),
                    slot: series[*series_index].slot,
                    title: if series.len() > 1 {
                        format!("{name}, {}: {}", bar.label, format(*value))
                    } else {
                        format!("{}: {}", bar.label, format(*value))
                    },
                });
            }
            Bar {
                center: coord(x + width / 2.0),
                label: (index % label_every == 0).then(|| bar.short_label.clone()),
                segments,
            }
        })
        .collect();

    let visible: Vec<usize> = (0..series.len()).filter(|i| shown[*i]).collect();
    let legend = if visible.len() > 1 {
        visible
            .iter()
            .map(|i| LegendItem {
                name: series[*i].name.clone(),
                slot: series[*i].slot,
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut table_head = vec![String::new()];
    table_head.extend(visible.iter().map(|i| series[*i].name.clone()));
    let table_rows = bars
        .iter()
        .map(|bar| {
            let mut row = vec![bar.label.clone()];
            row.extend(
                visible
                    .iter()
                    .map(|i| format(bar.values.get(*i).copied().unwrap_or(0.0))),
            );
            row
        })
        .collect();

    Some(BarChart {
        view_box: format!("0 0 {WIDTH} {HEIGHT}"),
        description,
        grid,
        bars: drawn,
        plot_left: coord(LEFT),
        plot_right: coord(WIDTH - RIGHT),
        tick_x: coord(LEFT - 8.0),
        label_y: coord(HEIGHT - 10.0),
        legend,
        table_head,
        table_rows,
    })
}

/// A round step (1, 2, 2.5 or 5 times a power of ten) and how many of them reach `max`, aiming
/// for about four gridlines.
fn ticks(max: f64) -> (f64, u32) {
    let raw = max / 4.0;
    let magnitude = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|m| m * magnitude)
        .find(|step| *step >= raw)
        .unwrap_or(10.0 * magnitude);
    (step, (max / step).ceil().max(1.0) as u32)
}

/// Square at the baseline, rounded at the top when `radius` is non-zero.
fn bar_path(x: f64, y: f64, width: f64, height: f64, radius: f64) -> String {
    let r = radius.min(height).min(width / 2.0);
    let (left, right, bottom) = (coord(x), coord(x + width), coord(y + height));
    if r <= 0.0 {
        return format!("M{left} {}H{right}V{bottom}H{left}Z", coord(y));
    }
    format!(
        "M{left} {bottom}V{y_r}Q{left} {top} {x_r} {top}H{x_wr}Q{right} {top} {right} {y_r}V{bottom}Z",
        top = coord(y),
        y_r = coord(y + r),
        x_r = coord(x + r),
        x_wr = coord(x + width - r),
    )
}

/// SVG coordinates to a tenth of a unit, without trailing zeros.
fn coord(value: f64) -> String {
    format!("{}", (value * 10.0).round() / 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(values: &[f64]) -> BarInput {
        BarInput {
            short_label: "Mar".into(),
            label: "March 2026".into(),
            values: values.to_vec(),
        }
    }

    fn series(names: &[&str]) -> Vec<Series> {
        names
            .iter()
            .zip(SLOTS)
            .map(|(name, slot)| Series {
                name: (*name).to_owned(),
                slot,
            })
            .collect()
    }

    fn grams(value: f64) -> String {
        format!("{value:.0} g")
    }

    #[test]
    fn ticks_are_round_and_cover_the_maximum() {
        assert_eq!(ticks(412.0), (200.0, 3));
        assert_eq!(ticks(1000.0), (250.0, 4));
        assert_eq!(ticks(3.0), (1.0, 3));
    }

    #[test]
    fn only_the_top_segment_is_rounded_and_segments_are_gapped() {
        let chart = stacked(
            String::new(),
            series(&["sam", "alex", "kim"]),
            vec![bar(&[100.0, 0.0, 100.0]), bar(&[0.0, 0.0, 0.0])],
            grams,
        )
        .unwrap();
        let segments = &chart.bars[0].segments;
        assert_eq!(segments.len(), 2, "zero values draw nothing");
        assert!(segments[0].path.ends_with('Z') && !segments[0].path.contains('Q'));
        assert!(segments[1].path.contains('Q'));
        assert_eq!(segments[1].slot, "series-3", "the slot follows the series");
        assert_eq!(segments[1].title, "kim, March 2026: 100 g");
        assert!(chart.bars[1].segments.is_empty());

        // Two bands of 442 centre the bars at x 273; 200 g fills the 218 high plot, so 100 g
        // is 109, and the upper segment starts 2 above the lower one's top.
        assert_eq!(segments[0].path, "M273 121H297V230H273Z");
        assert_eq!(
            segments[1].path,
            "M273 119V16Q273 12 277 12H293Q297 12 297 16V119Z"
        );

        let names: Vec<&str> = chart.legend.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["sam", "kim"], "series without values stay out");
        assert_eq!(chart.table_head, ["", "sam", "kim"]);
        assert_eq!(chart.table_rows[1], ["March 2026", "0 g", "0 g"]);
    }

    #[test]
    fn empty_charts_and_single_series_legends_are_left_out() {
        assert!(stacked(String::new(), series(&["sam"]), vec![bar(&[0.0])], grams).is_none());
        let single = stacked(String::new(), series(&["sam"]), vec![bar(&[5.0])], grams).unwrap();
        assert!(single.legend.is_empty());
        assert_eq!(single.bars[0].segments[0].title, "March 2026: 5 g");
    }

    #[test]
    fn labels_thin_out_on_long_ranges() {
        let bars = (0..31).map(|_| bar(&[1.0])).collect();
        let chart = stacked(String::new(), series(&["sam"]), bars, grams).unwrap();
        let labelled = chart.bars.iter().filter(|bar| bar.label.is_some()).count();
        assert_eq!(labelled, 11, "every third of 31");
    }
}
