//! Criterion benchmarks for the engine's hot paths.
//!
//! Everything here runs on synthetic fixtures - no real WAD or map files
//! are needed, and the fixtures stay tiny on purpose: these measure the
//! inner loops, not content loading. Sample counts and measurement times
//! are kept low so `cargo bench` stays a smoke test you can actually run.

use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

use doom_engine::bsp::{ClipSegList, ViewState, ViewTables, traverse_bsp};
use doom_engine::fixed::Fixed;
use doom_engine::gfx::flat::Flat;
use doom_engine::gfx::palette::Colormap;
use doom_engine::map::types::NF_SUBSECTOR;
use doom_engine::map::{Node, NodeChild};
use doom_engine::render::{
    ColumnParams, IndexedFramebuffer, LightTables, MAXLIGHTSCALE, SCREEN_HEIGHT, SCREEN_WIDTH,
    SpanParams, SpriteDefinitions, SpriteLumpTables, SpriteThing, WallProjection, WallView,
    WrappedColumn, draw_column, draw_span, project_sprite,
};
use doom_engine::resources::ResourceManager;
use doom_engine::tables::{Angle, FINE_COSINE, FINE_SINE};
use doom_engine::wad::{WadFile, WadSet};

const VIEW_WIDTH: i32 = 320;
const VIEW_HEIGHT: i32 = 200;

fn bench_config() -> Criterion {
    Criterion::default()
        .measurement_time(Duration::from_secs(2))
        .warm_up_time(Duration::from_secs(1))
        .sample_size(100)
}

/// Deterministic xor-ish fixture source; no need for a real PRNG here.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
}

// ------------------------------------------------------------------
// synthetic WAD (testutil is cfg(test)-only, so benches build their own)
// ------------------------------------------------------------------

fn build_wad(lumps: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"IWAD");
    out.extend_from_slice(&(lumps.len() as i32).to_le_bytes());
    let data_start = 12 + lumps.iter().map(|(_, data)| data.len()).sum::<usize>();
    out.extend_from_slice(&(data_start as i32).to_le_bytes());

    let mut records = Vec::with_capacity(lumps.len());
    for (name, data) in lumps {
        let file_pos = out.len();
        out.extend_from_slice(data);
        let mut name_bytes = [0u8; 8];
        for (dst, src) in name_bytes.iter_mut().zip(name.as_bytes().iter().take(8)) {
            *dst = *src;
        }
        records.push((file_pos as i32, data.len() as i32, name_bytes));
    }
    for (file_pos, size, name) in records {
        out.extend_from_slice(&file_pos.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&name);
    }
    out
}

fn synthetic_wad_bytes(lump_count: usize) -> Vec<u8> {
    let lumps: Vec<(String, Vec<u8>)> = (0..lump_count)
        .map(|i| (format!("LUMP{i:04}"), vec![(i & 0xff) as u8; 16]))
        .collect();
    let refs: Vec<(&str, Vec<u8>)> = lumps
        .iter()
        .map(|(name, data)| (name.as_str(), data.clone()))
        .collect();
    build_wad(&refs)
}

// ------------------------------------------------------------------
// fixed-point arithmetic
// ------------------------------------------------------------------

fn bench_fixed(c: &mut Criterion) {
    let mut rng = Lcg(0x1234_5678_9abc_def0);
    let pairs: Vec<(Fixed, Fixed)> = (0..1024)
        .map(|_| {
            (
                Fixed::from_raw(rng.next() as i32),
                Fixed::from_raw(rng.next() as i32 | 1),
            )
        })
        .collect();

    let mut group = c.benchmark_group("fixed");
    group.bench_function("fixed_mul", |b| {
        b.iter(|| {
            let mut acc = Fixed::ONE;
            for &(x, y) in &pairs {
                acc = black_box(black_box(acc * black_box(x)) * black_box(y));
            }
            acc
        })
    });
    group.bench_function("fixed_div", |b| {
        b.iter(|| {
            let mut acc = Fixed::from_int(1 << 14);
            for &(x, y) in &pairs {
                acc = black_box(black_box(acc / black_box(y)) + black_box(x));
            }
            acc
        })
    });
    group.finish();
}

// ------------------------------------------------------------------
// wad directory parse + lump lookup
// ------------------------------------------------------------------

fn bench_wad(c: &mut Criterion) {
    let bytes = synthetic_wad_bytes(256);
    let mut set = WadSet::new();
    set.add_wad_bytes(bytes.clone())
        .expect("synthetic wad parses");

    let mut group = c.benchmark_group("wad");
    group.bench_function("parse_directory", |b| {
        b.iter_batched(
            || bytes.clone(),
            |wad_bytes| WadFile::from_bytes(black_box(wad_bytes)).map(|wad| wad.lump_count()),
            BatchSize::SmallInput,
        )
    });
    group.bench_function("find_lump_hit_tail", |b| {
        // present at the end of the namespace: the scan-from-behind lookup
        // finds it on the first probe
        b.iter(|| black_box(set.find_lump(black_box("LUMP0255"))))
    });
    group.bench_function("find_lump_miss_full_scan", |b| {
        // worst case: scans the whole directory and finds nothing
        b.iter(|| black_box(set.find_lump(black_box("NOSUCHL"))))
    });
    group.finish();
}

// ------------------------------------------------------------------
// bsp traversal + clip-segment list
// ------------------------------------------------------------------

const NODE_TREE_DEPTH: u32 = 8;
const BIG_BBOX: [[Fixed; 4]; 2] = [
    [
        Fixed::from_int(16384),
        Fixed::from_int(-16384),
        Fixed::from_int(-16384),
        Fixed::from_int(16384),
    ],
    [
        Fixed::from_int(16384),
        Fixed::from_int(-16384),
        Fixed::from_int(-16384),
        Fixed::from_int(16384),
    ],
];

/// Builds a balanced synthetic tree bottom-up; every bbox encloses the
/// world, so `check_bbox` never culls and the traversal walks everything.
fn build_node_tree(depth: u32) -> (Vec<Node>, NodeChild) {
    fn recur(nodes: &mut Vec<Node>, depth: u32, leaf: &mut usize) -> u16 {
        if depth == 0 {
            let index = *leaf;
            *leaf += 1;
            return NF_SUBSECTOR | index as u16;
        }
        let index = nodes.len();
        nodes.push(Node {
            x: Fixed::ZERO,
            y: Fixed::ZERO,
            dx: Fixed::ZERO,
            dy: Fixed::ONE,
            bbox: BIG_BBOX,
            children: [0, 0],
        });
        let left = recur(nodes, depth - 1, leaf);
        let right = recur(nodes, depth - 1, leaf);
        nodes[index].children = [left, right];
        index as u16
    }

    let mut nodes = Vec::new();
    let mut leaf = 0usize;
    let root = recur(&mut nodes, depth, &mut leaf);
    (nodes, NodeChild::Node(root as usize))
}

fn bench_bsp(c: &mut Criterion) {
    let (nodes, root) = build_node_tree(NODE_TREE_DEPTH);
    let tables = ViewTables::new(VIEW_WIDTH, Fixed::from_int(VIEW_WIDTH / 2));
    let view = ViewState::new(Fixed::ZERO, Fixed::ZERO, Angle::ZERO);

    // spans for the clip benches: a stable pseudo-random script of wall
    // ranges across the 320-column view
    let mut rng = Lcg(0x0bad_c0de_dead_beef);
    let spans: Vec<(i32, i32)> = (0..64)
        .map(|_| {
            let first = (rng.next() % 280) as i32;
            (first, first + (rng.next() % 40) as i32)
        })
        .collect();

    let mut group = c.benchmark_group("bsp");
    group.bench_function("traverse_tree", |b| {
        b.iter(|| {
            let mut clip = ClipSegList::cleared(VIEW_WIDTH);
            let mut visited = 0usize;
            traverse_bsp(
                black_box(&nodes),
                root,
                black_box(&view),
                &tables,
                &mut clip,
                &mut |_clip: &mut ClipSegList, _subsector: usize| visited += 1,
            );
            black_box(visited)
        })
    });
    group.bench_function("clip_solid_wall_segment", |b| {
        b.iter_batched(
            || ClipSegList::cleared(VIEW_WIDTH),
            |mut clip| {
                for &(first, last) in &spans {
                    clip.clip_solid_wall_segment(black_box(first), black_box(last), |_, _| {});
                }
                black_box(clip.len())
            },
            BatchSize::SmallInput,
        )
    });
    group.bench_function("clip_pass_wall_segment", |b| {
        b.iter_batched(
            || ClipSegList::cleared(VIEW_WIDTH),
            |mut clip| {
                for &(first, last) in &spans {
                    clip.clip_pass_wall_segment(black_box(first), black_box(last), |_, _| {});
                }
                black_box(clip.len())
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

// ------------------------------------------------------------------
// column / span rasterizers
// ------------------------------------------------------------------

fn bench_draw(c: &mut Criterion) {
    let mut fb = IndexedFramebuffer::new();
    let column_bytes = [7u8; 128];
    let source = WrappedColumn::wall(&column_bytes);
    let colormap = Colormap(core::array::from_fn(|i| i as u8));
    let column_params = ColumnParams {
        colormap: &colormap,
        x: 160,
        y_lo: 0,
        y_hi: SCREEN_HEIGHT as i32 - 1,
        iscale: Fixed::from_int(1),
        texturemid: Fixed::from_int(100),
        center_y: VIEW_HEIGHT / 2,
    };
    let flat = Flat([13u8; 4096]);
    let span_params = SpanParams {
        colormap: &colormap,
        y: 100,
        x_lo: 0,
        x_hi: SCREEN_WIDTH as i32 - 1,
        x_frac: Fixed::ZERO,
        y_frac: Fixed::ZERO,
        x_step: Fixed::ONE,
        y_step: Fixed::ONE,
    };

    let mut group = c.benchmark_group("draw");
    group.bench_function("draw_column", |b| {
        b.iter(|| {
            draw_column(
                black_box(&mut fb),
                black_box(&source),
                black_box(&column_params),
            )
        })
    });
    group.bench_function("draw_span", |b| {
        b.iter(|| {
            draw_span(
                black_box(&mut fb),
                black_box(&flat),
                black_box(&span_params),
            )
        })
    });
    group.finish();
}

// ------------------------------------------------------------------
// sprite projection (R_ProjectSprite analog)
// ------------------------------------------------------------------

fn patch_bytes(
    width: i16,
    height: i16,
    left_offset: i16,
    top_offset: i16,
    columns: &[Vec<(u8, Vec<u8>)>],
) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&width.to_le_bytes());
    data.extend_from_slice(&height.to_le_bytes());
    data.extend_from_slice(&left_offset.to_le_bytes());
    data.extend_from_slice(&top_offset.to_le_bytes());
    let mut ptrs = Vec::with_capacity(width as usize);
    let mut body = Vec::new();
    let mut offset = 8 + width as usize * 4;
    for column in columns {
        ptrs.push(offset as i32);
        for (top_delta, pixels) in column {
            body.extend_from_slice(&[*top_delta, pixels.len() as u8, 0]);
            body.extend_from_slice(pixels);
            body.push(0);
            offset += 4 + pixels.len();
        }
        body.push(0xff);
        offset += 1;
    }
    for ptr in &ptrs {
        data.extend_from_slice(&ptr.to_le_bytes());
    }
    data.extend_from_slice(&body);
    data
}

fn simple_patch(width: i16, height: i16, fill: u8) -> Vec<u8> {
    let columns = (0..width)
        .map(|_| vec![(0u8, vec![fill; height as usize])])
        .collect::<Vec<_>>();
    patch_bytes(width, height, width / 2, height, &columns)
}

fn bench_sprite_projection(c: &mut Criterion) {
    let pnames = 0i32.to_le_bytes().to_vec();
    let texture1 = 0i32.to_le_bytes().to_vec();
    let patch = simple_patch(8, 12, 3);
    let lumps: Vec<(&str, &[u8])> = vec![
        ("PNAMES", pnames.as_slice()),
        ("TEXTURE1", texture1.as_slice()),
        ("S_START", &[]),
        ("POSSA0", patch.as_slice()),
        ("S_END", &[]),
    ];
    let wad_bytes = build_wad(
        &lumps
            .iter()
            .map(|(name, data)| (*name, data.to_vec()))
            .collect::<Vec<_>>(),
    );
    let manager = ResourceManager::new(
        WadFile::from_bytes(wad_bytes).expect("synthetic sprite wad parses"),
        Vec::new(),
    );
    let sprite_lumps = SpriteLumpTables::build(&manager).expect("sprite lumps build");
    let defs = SpriteDefinitions::build(&[*b"POSS"], &manager).expect("sprite defs build");
    let tables = ViewTables::new(VIEW_WIDTH, Fixed::from_int(VIEW_WIDTH / 2));
    let projection = WallProjection::build(&tables, VIEW_WIDTH);
    let light = LightTables::build(VIEW_WIDTH);
    let textures =
        doom_engine::render::TextureSet::from_resources(&manager).expect("texture set builds");
    let colormaps: Vec<Colormap> = (0..33)
        .map(|_| Colormap(core::array::from_fn(|i| i as u8)))
        .collect();
    let angle = Angle::ZERO;
    let view = WallView {
        tables: &tables,
        projection: &projection,
        light: &light,
        textures: &textures,
        colormaps: &colormaps,
        view_x: Fixed::ZERO,
        view_y: Fixed::ZERO,
        view_z: Fixed::ZERO,
        view_angle: angle,
        view_cos: FINE_COSINE[angle.fine_angle()],
        view_sin: FINE_SINE[angle.fine_angle()],
        view_width: VIEW_WIDTH,
        view_height: VIEW_HEIGHT,
        center_y: VIEW_HEIGHT / 2,
        center_x_frac: Fixed::from_int(VIEW_WIDTH / 2),
        center_y_frac: Fixed::from_int(VIEW_HEIGHT / 2),
        projection_distance: Fixed::from_int(VIEW_WIDTH / 2),
        extralight: 0,
        fixed_colormap: None,
        detail_shift: 0,
        sky_flat: None,
    };
    let spritelights = [7usize; MAXLIGHTSCALE];
    let thing = SpriteThing {
        x: Fixed::from_int(100),
        y: Fixed::ZERO,
        z: Fixed::ZERO,
        angle: Angle::ZERO,
        sprite: 0,
        frame: 0,
        flags: 0,
    };

    c.bench_function("sprite/project_sprite", |b| {
        b.iter(|| {
            black_box(project_sprite(
                black_box(&thing),
                black_box(&view),
                &defs,
                &sprite_lumps,
                black_box(&spritelights),
            ))
        })
    });
}

// ------------------------------------------------------------------
// zlight/scalelight table build
// ------------------------------------------------------------------

fn bench_light_tables(c: &mut Criterion) {
    c.bench_function("light/light_tables_build", |b| {
        b.iter(|| black_box(LightTables::build(black_box(VIEW_WIDTH))))
    });
}

criterion_group! {
    name = benches;
    config = bench_config();
    targets =
        bench_fixed,
        bench_wad,
        bench_bsp,
        bench_draw,
        bench_sprite_projection,
        bench_light_tables,
}
criterion_main!(benches);
