use crate::services::{
    ReadOnlyService, ServiceEvent,
    compositor::{CompositorService, CompositorWindow},
};
use iced::{
    Alignment, Color, Element, Length, Point, Rectangle, Renderer, Size, Subscription, Theme,
    mouse::Cursor,
    widget::{
        canvas,
        canvas::{Frame, Geometry, Path, Program, Stroke},
        container,
    },
};
use std::collections::BTreeMap;

// Target minimap size in pixels; the layout is scaled to fit within it.
const MAX_H: f32 = 16.0;
const MAX_W: f32 = 80.0;
const MIN_TILE: f32 = 2.0;
// Gap carved out of a tile's edge where it abuts a neighbour, so adjacent
// same-colour tiles don't merge into one block.
const TILE_GAP: f32 = 1.0;
const VIEWPORT_STROKE_W: f32 = 1.0;
const FLOATING_STROKE_W: f32 = 1.0;
// Tolerance for "shared edge" — coordinates come from compositor floats, so
// allow sub-pixel slack.
const EPS: f32 = 0.5;

/// An axis-aligned rectangle `(x, y, w, h)` in some 2D space.
type Rect = (f32, f32, f32, f32);

#[derive(Debug, Clone)]
pub enum Message {
    ServiceEvent(ServiceEvent<CompositorService>),
}

pub struct Minimap {
    service: Option<CompositorService>,
}

impl Minimap {
    pub fn new() -> Self {
        Self { service: None }
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::ServiceEvent(event) => match event {
                ServiceEvent::Init(service) => {
                    self.service = Some(service);
                }
                ServiceEvent::Update(event) => {
                    if let Some(service) = &mut self.service {
                        service.update(event);
                    }
                }
                _ => {}
            },
        }
    }

    pub fn view(&self) -> Option<Element<'_, Message>> {
        let service = self.service.as_ref()?;
        let active_id = service.active_workspace_id?;
        let windows: Vec<&CompositorWindow> = service
            .windows
            .iter()
            .filter(|w| w.workspace_id == Some(active_id))
            .collect();

        let minimap = build_canvas(&windows, viewport_rect(service, active_id))?;

        let (w, h) = (minimap.width, minimap.height);
        Some(
            container(
                canvas(minimap)
                    .width(Length::Fixed(w))
                    .height(Length::Fixed(h)),
            )
            .align_y(Alignment::Center)
            .into(),
        )
    }

    pub fn subscription(&self) -> Subscription<Message> {
        CompositorService::subscribe().map(Message::ServiceEvent)
    }
}

/// The output's on-screen viewport, in workspace-layout coordinates:
/// `(view_point.x, view_point.y, width, height)`. `None` unless the
/// compositor exposes both the view point and the monitor's logical size.
fn viewport_rect(service: &CompositorService, ws_id: i32) -> Option<Rect> {
    let ws = service.workspaces.iter().find(|w| w.id == ws_id)?;
    let mon = service.monitors.iter().find(|m| m.name == ws.monitor)?;
    let (px, py) = mon.view_point?;
    let (w, h) = mon.logical_size?;
    Some((px, py, w as f32, h as f32))
}

#[derive(Debug, Clone, Copy)]
enum TileRole {
    Normal,
    Focused,
    Urgent,
}

fn role_of(w: &CompositorWindow) -> TileRole {
    if w.is_focused {
        TileRole::Focused
    } else if w.is_urgent {
        TileRole::Urgent
    } else {
        TileRole::Normal
    }
}

fn tile_color(role: TileRole, theme: &Theme) -> Color {
    match role {
        TileRole::Focused => theme.palette().primary,
        TileRole::Urgent => theme.palette().danger,
        TileRole::Normal => theme.extended_palette().background.strong.color,
    }
}

#[derive(Debug, Clone, Copy)]
enum Shape {
    /// Tiled window: a filled rectangle, inset on sides with a neighbour.
    Tile,
    /// Floating window placed in the layout: filled rectangle with a halo,
    /// drawn on top of the tiling.
    Floating,
}

#[derive(Debug, Clone, Copy)]
struct CanvasTile {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    inset_r: f32,
    inset_b: f32,
    role: TileRole,
    shape: Shape,
}

/// A fully positioned minimap, in canvas (post-scale) pixel coordinates.
struct MinimapCanvas {
    tiles: Vec<CanvasTile>,
    viewport: Option<Rect>,
    width: f32,
    height: f32,
}

impl<Message> Program<Message> for MinimapCanvas {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());

        // Base layer: tiled windows. Insets shrink a tile only on sides with
        // a neighbour, so adjacent tiles stay separated while outer edges stay
        // flush with the viewport.
        for t in &self.tiles {
            if !matches!(t.shape, Shape::Tile) {
                continue;
            }
            frame.fill_rectangle(
                Point::new(t.x, t.y),
                Size::new(
                    (t.w - t.inset_r).max(MIN_TILE),
                    (t.h - t.inset_b).max(MIN_TILE),
                ),
                tile_color(t.role, theme),
            );
        }

        // Viewport outline over the tiles. Stroked rectangles centre the
        // stroke on the path, so nudge the rect inward to keep it aligned
        // with the canvas extent.
        if let Some((vx, vy, vw, vh)) = self.viewport {
            let inset = VIEWPORT_STROKE_W / 2.0;
            let path = Path::rectangle(
                Point::new(vx + inset, vy + inset),
                Size::new(
                    (vw - VIEWPORT_STROKE_W).max(0.0),
                    (vh - VIEWPORT_STROKE_W).max(0.0),
                ),
            );
            frame.stroke(
                &path,
                Stroke::default()
                    .with_width(VIEWPORT_STROKE_W)
                    .with_color(theme.palette().text),
            );
        }

        // Floating windows on top of everything. A halo in the canvas
        // background colour separates the floating fill from inactive tiles
        // beneath it, which would otherwise share the same colour and merge.
        let halo = theme.extended_palette().background.base.color;
        for t in &self.tiles {
            if !matches!(t.shape, Shape::Floating) {
                continue;
            }
            frame.fill_rectangle(
                Point::new(t.x - FLOATING_STROKE_W, t.y - FLOATING_STROKE_W),
                Size::new(t.w + 2.0 * FLOATING_STROKE_W, t.h + 2.0 * FLOATING_STROKE_W),
                halo,
            );
            frame.fill_rectangle(
                Point::new(t.x, t.y),
                Size::new(t.w, t.h),
                tile_color(t.role, theme),
            );
        }

        vec![frame.into_geometry()]
    }
}

fn has_neighbor_right(rects: &[Rect], x: f32, y: f32, w: f32, h: f32) -> bool {
    rects
        .iter()
        .any(|&(ox, oy, _, oh)| (ox - (x + w)).abs() < EPS && oy < y + h - EPS && oy + oh > y + EPS)
}

fn has_neighbor_below(rects: &[Rect], x: f32, y: f32, w: f32, h: f32) -> bool {
    rects
        .iter()
        .any(|&(ox, oy, ow, _)| (oy - (y + h)).abs() < EPS && ox < x + w - EPS && ox + ow > x + EPS)
}

/// Lay tiled windows out in workspace-layout space (column 1 at x = 0):
/// columns left to right by cumulative max width, tiles top to bottom within
/// a column by cumulative height. Positions are derived from the grid index.
fn layout_tiled<'a>(tiled: &[&'a CompositorWindow]) -> Vec<(Rect, &'a CompositorWindow)> {
    let mut by_col: BTreeMap<u32, Vec<&CompositorWindow>> = BTreeMap::new();
    for w in tiled {
        let (col, _) = w.tile_position.unwrap_or((0, 0));
        by_col.entry(col).or_default().push(w);
    }
    for tiles in by_col.values_mut() {
        tiles.sort_by_key(|w| w.tile_position.map(|(_, r)| r).unwrap_or(0));
    }

    let mut placed = Vec::new();
    let mut x = 0.0_f32;
    for tiles in by_col.values() {
        let col_w = tiles
            .iter()
            .map(|w| w.tile_width)
            .fold(0.0_f32, f32::max)
            .max(1.0);
        let mut y = 0.0_f32;
        for w in tiles {
            let h = w.tile_height.max(1.0);
            placed.push(((x, y, col_w, h), *w));
            y += h;
        }
        x += col_w;
    }
    placed
}

/// Build the minimap in one workspace-layout coordinate space: tiled windows
/// from their grid index, floating windows from their absolute `floating_pos`,
/// and the output's viewport drawn as an outlined rectangle. Returns `None`
/// only when there are neither windows nor a viewport to draw.
fn build_canvas(windows: &[&CompositorWindow], viewport: Option<Rect>) -> Option<MinimapCanvas> {
    let tiled: Vec<&CompositorWindow> =
        windows.iter().copied().filter(|w| !w.is_floating).collect();
    let placed = layout_tiled(&tiled);

    // Layout-space rects tagged with how to draw them.
    let mut items: Vec<(Rect, TileRole, Shape)> = placed
        .iter()
        .map(|(r, w)| (*r, role_of(w), Shape::Tile))
        .collect();
    for w in windows.iter().copied().filter(|w| w.is_floating) {
        if let Some((x, y)) = w.floating_pos {
            items.push((
                (x, y, w.tile_width, w.tile_height),
                role_of(w),
                Shape::Floating,
            ));
        }
    }
    // Nothing to draw only when there are no windows and no viewport; an empty
    // workspace still shows its viewport rectangle.
    if items.is_empty() && viewport.is_none() {
        return None;
    }

    // Bounding box over everything placed in layout space (windows + viewport).
    let mut bounds: Vec<Rect> = items.iter().map(|(r, _, _)| *r).collect();
    bounds.extend(viewport);
    let min_x = bounds.iter().map(|r| r.0).fold(f32::MAX, f32::min);
    let min_y = bounds.iter().map(|r| r.1).fold(f32::MAX, f32::min);
    let max_x = bounds.iter().map(|r| r.0 + r.2).fold(f32::MIN, f32::max);
    let max_y = bounds.iter().map(|r| r.1 + r.3).fold(f32::MIN, f32::max);
    let layout_w = (max_x - min_x).max(1.0);
    let layout_h = (max_y - min_y).max(1.0);
    let scale = (MAX_H / layout_h).min(MAX_W / layout_w);

    let tiled_rects: Vec<Rect> = items
        .iter()
        .filter(|(_, _, s)| matches!(s, Shape::Tile))
        .map(|(r, _, _)| *r)
        .collect();

    let tiles: Vec<CanvasTile> = items
        .iter()
        .map(|&((x, y, w, h), role, shape)| {
            let (inset_r, inset_b) = match shape {
                Shape::Tile => (
                    if has_neighbor_right(&tiled_rects, x, y, w, h) {
                        TILE_GAP
                    } else {
                        0.0
                    },
                    if has_neighbor_below(&tiled_rects, x, y, w, h) {
                        TILE_GAP
                    } else {
                        0.0
                    },
                ),
                Shape::Floating => (0.0, 0.0),
            };
            CanvasTile {
                x: (x - min_x) * scale,
                y: (y - min_y) * scale,
                w: (w * scale).max(MIN_TILE),
                h: (h * scale).max(MIN_TILE),
                inset_r,
                inset_b,
                role,
                shape,
            }
        })
        .collect();

    let viewport = viewport.map(|(x, y, w, h)| {
        (
            (x - min_x) * scale,
            (y - min_y) * scale,
            w * scale,
            h * scale,
        )
    });

    Some(MinimapCanvas {
        tiles,
        viewport,
        width: (layout_w * scale).max(1.0),
        height: (layout_h * scale).max(1.0),
    })
}
