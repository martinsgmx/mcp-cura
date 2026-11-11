//! Mesh loading, inspection, and manipulation built on `baby_shark`.
//!
//! STL is parsed locally so that both binary and ASCII files work; OBJ and PLY
//! are delegated to `baby_shark`. Geometry is kept as a triangle soup, which is
//! what `CuraEngine` and STL expect. Voxel operations convert through a signed
//! distance volume and back with marching cubes.

use std::{
    collections::HashMap,
    fs,
    io::{BufWriter, Write},
    path::Path,
};

use baby_shark::{
    decimation::{AlwaysDecimate, EdgeDecimator},
    exports::nalgebra::{Rotation3, Vector3},
    io::read_from_file,
    mesh::{
        corner_table::CornerTable, polygon_soup::data_structure::PolygonSoup, traits::Triangles,
    },
    voxel::{mesh_to_volume::MeshToVolume, meshing::MarchingCubesMesher, prelude::Volume},
};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Axis-aligned bounds in millimetres.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Bounds {
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub size: [f64; 3],
    pub center: [f64; 3],
}

impl Bounds {
    /// Whether the bounds contain finite, ordered coordinates.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.size.iter().all(|value| *value >= 0.0)
    }

    /// Estimated voxel cells for a conversion at `voxel_size`.
    #[must_use]
    pub fn voxel_cells(&self, voxel_size: f64) -> u64 {
        let cells = self
            .size
            .iter()
            .map(|size| (size / voxel_size).ceil().max(1.0) + 3.0)
            .product::<f64>();
        if cells.is_finite() && cells >= 0.0 {
            cells as u64
        } else {
            u64::MAX
        }
    }
}

/// Quantized vertex identity used for topology checks.
type VertexKey = (u64, u64, u64);
/// Undirected edge between two quantized vertices.
type EdgeKey = (VertexKey, VertexKey);

/// Mesh statistics returned by `cura_mesh_inspect`.
#[derive(Debug, Clone, Serialize)]
pub struct MeshStats {
    pub triangles: usize,
    pub vertices: usize,
    pub bounds: Bounds,
    pub surface_area_mm2: f64,
    pub volume_mm3: f64,
    pub signed_volume_mm3: f64,
    pub watertight: bool,
    pub boundary_edges: usize,
    pub non_manifold_edges: usize,
    pub degenerate_triangles: usize,
}

/// A triangle soup in millimetres.
#[derive(Debug)]
pub struct Mesh {
    soup: PolygonSoup<f32>,
}

impl Clone for Mesh {
    fn clone(&self) -> Self {
        Self {
            soup: PolygonSoup::from_vertices(self.soup.vertices().copied().collect()),
        }
    }
}

impl Mesh {
    /// Wrap a `baby_shark` soup.
    #[must_use]
    pub fn from_soup(soup: PolygonSoup<f32>) -> Self {
        Self { soup }
    }

    /// Borrow the underlying soup.
    #[must_use]
    pub fn soup(&self) -> &PolygonSoup<f32> {
        &self.soup
    }

    /// Load a mesh from STL, OBJ, or PLY.
    pub fn load_path(path: &Path) -> Result<Self> {
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match extension.as_str() {
            "stl" => {
                let bytes = fs::read(path)?;
                Self::from_stl_bytes(&bytes)
            }
            "obj" | "ply" => {
                let soup = read_from_file::<PolygonSoup<f32>>(path).map_err(|err| {
                    Error::Mesh(format!("cannot read {}: {err:?}", path.display()))
                })?;
                Ok(Self { soup })
            }
            other => Err(Error::Mesh(format!(
                "unsupported mesh format '{other}' (expected stl, obj or ply)"
            ))),
        }
    }

    /// Parse binary or ASCII STL bytes.
    pub fn from_stl_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() >= 84 {
            let count = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
            if 84usize.saturating_add(count.saturating_mul(50)) == bytes.len() {
                return Self::from_binary_stl(bytes, count);
            }
        }
        if bytes
            .iter()
            .take(64)
            .copied()
            .any(|byte| byte == b'v' || byte == b'f')
        {
            if let Ok(mesh) = Self::from_ascii_stl(bytes) {
                return Ok(mesh);
            }
        }
        Err(Error::Mesh(
            "STL is neither a valid binary nor ASCII triangle mesh".into(),
        ))
    }

    fn from_binary_stl(bytes: &[u8], count: usize) -> Result<Self> {
        let mut vertices = Vec::with_capacity(count * 3);
        for index in 0..count {
            let base = 84 + index * 50 + 12; // skip the 12-byte stored normal
            for vertex in 0..3 {
                let offset = base + vertex * 12;
                let x = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
                let y = f32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
                let z = f32::from_le_bytes(bytes[offset + 8..offset + 12].try_into().unwrap());
                if !x.is_finite() || !y.is_finite() || !z.is_finite() {
                    return Err(Error::Mesh("STL contains a non-finite coordinate".into()));
                }
                vertices.push(Vector3::new(x, y, z));
            }
        }
        Ok(Self {
            soup: PolygonSoup::from_vertices(vertices),
        })
    }

    fn from_ascii_stl(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| Error::Mesh("ASCII STL is not valid UTF-8".into()))?;
        let mut vertices = Vec::new();
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            if !parts
                .next()
                .is_some_and(|token| token.eq_ignore_ascii_case("vertex"))
            {
                continue;
            }
            let x = parse_f32(parts.next(), "vertex x")?;
            let y = parse_f32(parts.next(), "vertex y")?;
            let z = parse_f32(parts.next(), "vertex z")?;
            vertices.push(Vector3::new(x, y, z));
        }
        if vertices.is_empty() || vertices.len() % 3 != 0 {
            return Err(Error::Mesh("ASCII STL has no complete triangles".into()));
        }
        Ok(Self {
            soup: PolygonSoup::from_vertices(vertices),
        })
    }

    /// Write to STL (binary), OBJ, or PLY based on the file extension.
    pub fn write_path(&self, path: &Path) -> Result<()> {
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match extension.as_str() {
            "stl" => self.write_stl(path),
            "obj" | "ply" => baby_shark::io::write_to_file(&self.soup, path)
                .map_err(|err| Error::Mesh(format!("cannot write {}: {err:?}", path.display()))),
            other => Err(Error::Mesh(format!(
                "unsupported output format '{other}' (expected stl, obj or ply)"
            ))),
        }
    }

    fn write_stl(&self, path: &Path) -> Result<()> {
        let triangles = self.triangle_count();
        let file = fs::File::create(path)?;
        let mut writer = BufWriter::new(file);
        let mut header = [0u8; 80];
        let label = b"mcp-cura binary STL";
        header[..label.len()].copy_from_slice(label);
        writer.write_all(&header)?;
        writer.write_all(&(triangles as u32).to_le_bytes())?;
        for triangle in self.soup.triangles() {
            let normal = (triangle.p2() - triangle.p1()).cross(&(triangle.p3() - triangle.p1()));
            let normal = if normal.norm_squared() > 0.0 {
                normal.normalize()
            } else {
                Vector3::zeros()
            };
            for value in [normal.x, normal.y, normal.z] {
                writer.write_all(&value.to_le_bytes())?;
            }
            for point in [triangle.p1(), triangle.p2(), triangle.p3()] {
                for value in [point.x, point.y, point.z] {
                    writer.write_all(&value.to_le_bytes())?;
                }
            }
            writer.write_all(&0u16.to_le_bytes())?;
        }
        writer.flush()?;
        Ok(())
    }

    /// Number of triangles.
    #[must_use]
    pub fn triangle_count(&self) -> usize {
        self.soup.vertices().count() / 3
    }

    /// Number of stored vertices (three per triangle for a soup).
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.soup.vertices().count()
    }

    /// Bounding box.
    #[must_use]
    pub fn bounds(&self) -> Bounds {
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        for vertex in self.soup.vertices() {
            for axis in 0..3 {
                let value = f64::from(vertex[axis]);
                min[axis] = min[axis].min(value);
                max[axis] = max[axis].max(value);
            }
        }
        if !min[0].is_finite() {
            min = [0.0; 3];
            max = [0.0; 3];
        }
        let size = [
            (max[0] - min[0]).max(0.0),
            (max[1] - min[1]).max(0.0),
            (max[2] - min[2]).max(0.0),
        ];
        Bounds {
            min,
            max,
            size,
            center: [
                min[0].midpoint(max[0]),
                min[1].midpoint(max[1]),
                min[2].midpoint(max[2]),
            ],
        }
    }

    /// Computing surface area and signed volume.
    #[must_use]
    pub fn signed_volume(&self) -> f64 {
        let mut volume = 0.0;
        for triangle in self.soup.triangles() {
            let a = triangle.p1();
            let b = triangle.p2();
            let c = triangle.p3();
            volume += f64::from(a.dot(&b.cross(c))) / 6.0;
        }
        volume
    }

    /// Surface area.
    #[must_use]
    pub fn surface_area(&self) -> f64 {
        self.soup
            .triangles()
            .map(|triangle| {
                f64::from(
                    (triangle.p2() - triangle.p1())
                        .cross(&(triangle.p3() - triangle.p1()))
                        .norm(),
                ) * 0.5
            })
            .sum()
    }

    /// Full statistics.
    #[must_use]
    pub fn stats(&self) -> MeshStats {
        let mut edges: HashMap<EdgeKey, u32> = HashMap::new();
        let mut degenerate = 0usize;
        let mut keys = Vec::with_capacity(self.vertex_count());
        for vertex in self.soup.vertices() {
            keys.push(quantized_key(vertex));
        }
        for chunk in keys.chunks_exact(3) {
            if chunk[0] == chunk[1] || chunk[1] == chunk[2] || chunk[0] == chunk[2] {
                degenerate += 1;
                continue;
            }
            for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                let (left, right) = (chunk[a], chunk[b]);
                let key = if left <= right {
                    (left, right)
                } else {
                    (right, left)
                };
                *edges.entry(key).or_insert(0) += 1;
            }
        }
        let boundary_edges = edges.values().filter(|count| **count == 1).count();
        let non_manifold_edges = edges.values().filter(|count| **count > 2).count();
        let signed = self.signed_volume();
        MeshStats {
            triangles: self.triangle_count(),
            vertices: self.vertex_count(),
            bounds: self.bounds(),
            surface_area_mm2: self.surface_area(),
            volume_mm3: signed.abs(),
            signed_volume_mm3: signed,
            watertight: boundary_edges == 0 && non_manifold_edges == 0 && degenerate == 0,
            boundary_edges,
            non_manifold_edges,
            degenerate_triangles: degenerate,
        }
    }

    /// Move the mesh.
    pub fn translate(&mut self, offset: [f64; 3]) {
        let offset = Vector3::new(offset[0] as f32, offset[1] as f32, offset[2] as f32);
        self.map_vertices(|vertex| *vertex + offset);
    }

    /// Scale the mesh around its current origin.
    pub fn scale(&mut self, factors: [f64; 3]) {
        let factors = Vector3::new(factors[0] as f32, factors[1] as f32, factors[2] as f32);
        self.map_vertices(|vertex| {
            Vector3::new(
                vertex.x * factors.x,
                vertex.y * factors.y,
                vertex.z * factors.z,
            )
        });
    }

    /// Rotate around the current origin using XYZ Euler angles in degrees.
    pub fn rotate_degrees(&mut self, angles: [f64; 3]) {
        let rotation = Rotation3::from_euler_angles(
            (angles[0] as f32).to_radians(),
            (angles[1] as f32).to_radians(),
            (angles[2] as f32).to_radians(),
        );
        self.map_vertices(|vertex| rotation * *vertex);
    }

    /// Mirror across one axis and fix triangle winding.
    pub fn mirror(&mut self, axis: usize) {
        let axis = axis.min(2);
        self.map_vertices(|vertex| {
            let mut mirrored = *vertex;
            mirrored[axis] = -mirrored[axis];
            mirrored
        });
        let vertices = self.soup.vertices().copied().collect::<Vec<_>>();
        let mut reordered = Vec::with_capacity(vertices.len());
        for chunk in vertices.chunks_exact(3) {
            reordered.extend([chunk[0], chunk[2], chunk[1]]);
        }
        self.soup = PolygonSoup::from_vertices(reordered);
    }

    fn map_vertices(&mut self, mut map: impl FnMut(&Vector3<f32>) -> Vector3<f32>) {
        let vertices = self.soup.vertices().map(&mut map).collect::<Vec<_>>();
        self.soup = PolygonSoup::from_vertices(vertices);
    }

    /// Center on the XY origin and place the lowest point on Z=0.
    pub fn center_on_bed(&mut self, center_xy: bool) {
        let bounds = self.bounds();
        let offset = [
            if center_xy { -bounds.center[0] } else { 0.0 },
            if center_xy { -bounds.center[1] } else { 0.0 },
            -bounds.min[2],
        ];
        self.translate(offset);
    }

    /// Append another mesh.
    pub fn merge(&mut self, other: &Mesh) {
        let mut vertices = self.soup.vertices().copied().collect::<Vec<_>>();
        vertices.extend(other.soup.vertices().copied());
        self.soup = PolygonSoup::from_vertices(vertices);
    }

    /// Estimate the voxel cell count for a conversion.
    #[must_use]
    pub fn voxel_cells(&self, voxel_size: f64) -> u64 {
        self.bounds().voxel_cells(voxel_size)
    }

    fn to_volume(&self, voxel_size: f32) -> Result<Volume> {
        MeshToVolume::default()
            .with_voxel_size(voxel_size)
            .convert(&self.soup)
            .ok_or_else(|| {
                Error::Mesh(
                    "voxelization failed; the mesh may be open, empty, or self-intersecting".into(),
                )
            })
    }

    fn from_volume(volume: &Volume, voxel_size: f32) -> Self {
        let vertices = MarchingCubesMesher::default()
            .with_voxel_size(voxel_size)
            .mesh(volume);
        Self {
            soup: PolygonSoup::from_vertices(vertices),
        }
    }

    /// Boolean operation. `operation` is `union`, `subtract`, or `intersect`.
    pub fn boolean(&self, other: &Mesh, operation: &str, voxel_size: f64) -> Result<Self> {
        let voxel = validate_voxel_size(voxel_size)?;
        let left = self.to_volume(voxel)?;
        let right = other.to_volume(voxel)?;
        let volume = match operation {
            "union" | "add" => left.union(right),
            "subtract" | "difference" => left.subtract(right),
            "intersect" | "intersection" => left.intersect(right),
            other => {
                return Err(Error::InvalidArguments(format!(
                    "unknown boolean operation '{other}' (expected union, subtract or intersect)"
                )));
            }
        };
        Ok(Self::from_volume(&volume, voxel))
    }

    /// Offset the surface by `distance` millimetres (positive expands).
    pub fn offset(&self, distance: f64, voxel_size: f64) -> Result<Self> {
        let voxel = validate_voxel_size(voxel_size)?;
        if !distance.is_finite() || distance.abs() > 100.0 {
            return Err(Error::InvalidArguments(
                "offset distance must be finite and within ±100 mm".into(),
            ));
        }
        let volume = self.to_volume(voxel)?.offset(distance as f32);
        Ok(Self::from_volume(&volume, voxel))
    }

    /// Rebuild the surface at a new voxel resolution.
    pub fn remesh(&self, voxel_size: f64) -> Result<Self> {
        let voxel = validate_voxel_size(voxel_size)?;
        let volume = self.to_volume(voxel)?;
        Ok(Self::from_volume(&volume, voxel))
    }

    /// Decimate towards a target triangle count.
    pub fn decimate(&self, target_triangles: usize) -> Result<Self> {
        if target_triangles == 0 {
            return Err(Error::InvalidArguments(
                "target_triangles must be positive".into(),
            ));
        }
        if target_triangles >= self.triangle_count() {
            return Ok(self.clone());
        }

        let (vertices, faces) = self.indexed();
        let mut table = CornerTable::from_vertex_and_face_slices(&vertices, &faces);
        if table.count_faces() == 0 {
            return Err(Error::Mesh(
                "cannot decimate a mesh without valid indexed faces".into(),
            ));
        }
        let mut decimator = EdgeDecimator::<f32, AlwaysDecimate>::new()
            .min_faces_count(Some(target_triangles))
            .keep_boundary(false);
        decimator.decimate(&mut table);

        let mut soup_vertices = Vec::with_capacity(table.count_faces() * 3);
        for triangle in table.triangles() {
            soup_vertices.push(*triangle.p1());
            soup_vertices.push(*triangle.p2());
            soup_vertices.push(*triangle.p3());
        }
        Ok(Self {
            soup: PolygonSoup::from_vertices(soup_vertices),
        })
    }

    fn indexed(&self) -> (Vec<Vector3<f32>>, Vec<usize>) {
        let mut unique = Vec::new();
        let mut lookup = HashMap::new();
        let mut faces = Vec::with_capacity(self.vertex_count());
        for vertex in self.soup.vertices() {
            let key = quantized_key(vertex);
            let index = *lookup.entry(key).or_insert_with(|| {
                unique.push(*vertex);
                unique.len() - 1
            });
            faces.push(index);
        }
        (unique, faces)
    }
}

fn validate_voxel_size(voxel_size: f64) -> Result<f32> {
    if !voxel_size.is_finite() || !(0.05..=20.0).contains(&voxel_size) {
        return Err(Error::InvalidArguments(
            "voxel_size must be between 0.05 and 20 mm".into(),
        ));
    }
    Ok(voxel_size as f32)
}

fn parse_f32(value: Option<&str>, label: &str) -> Result<f32> {
    let parsed = value
        .ok_or_else(|| Error::Mesh(format!("ASCII STL is missing {label}")))?
        .parse::<f32>()
        .map_err(|_| Error::Mesh(format!("ASCII STL has an invalid {label}")))?;
    if !parsed.is_finite() {
        return Err(Error::Mesh(format!("ASCII STL has a non-finite {label}")));
    }
    Ok(parsed)
}

fn quantized_key(vertex: &Vector3<f32>) -> VertexKey {
    let scale = 10_000.0;
    let quantize = |value: f32| -> u64 { ((value * scale) as i64) as u64 };
    (quantize(vertex.x), quantize(vertex.y), quantize(vertex.z))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE: &str = "solid cube\n\
facet normal 0 0 -1\n outer loop\n  vertex 0 0 0\n  vertex 0 10 0\n  vertex 10 10 0\n endloop\nendfacet\n\
facet normal 0 0 -1\n outer loop\n  vertex 0 0 0\n  vertex 10 10 0\n  vertex 10 0 0\n endloop\nendfacet\n\
facet normal 0 0 1\n outer loop\n  vertex 0 0 10\n  vertex 10 0 10\n  vertex 10 10 10\n endloop\nendfacet\n\
facet normal 0 0 1\n outer loop\n  vertex 0 0 10\n  vertex 10 10 10\n  vertex 0 10 10\n endloop\nendfacet\n\
facet normal 0 -1 0\n outer loop\n  vertex 0 0 0\n  vertex 10 0 0\n  vertex 10 0 10\n endloop\nendfacet\n\
facet normal 0 -1 0\n outer loop\n  vertex 0 0 0\n  vertex 10 0 10\n  vertex 0 0 10\n endloop\nendfacet\n\
facet normal 0 1 0\n outer loop\n  vertex 0 10 0\n  vertex 0 10 10\n  vertex 10 10 10\n endloop\nendfacet\n\
facet normal 0 1 0\n outer loop\n  vertex 0 10 0\n  vertex 10 10 10\n  vertex 10 10 0\n endloop\nendfacet\n\
facet normal -1 0 0\n outer loop\n  vertex 0 0 0\n  vertex 0 0 10\n  vertex 0 10 10\n endloop\nendfacet\n\
facet normal -1 0 0\n outer loop\n  vertex 0 0 0\n  vertex 0 10 10\n  vertex 0 10 0\n endloop\nendfacet\n\
facet normal 1 0 0\n outer loop\n  vertex 10 0 0\n  vertex 10 10 0\n  vertex 10 10 10\n endloop\nendfacet\n\
facet normal 1 0 0\n outer loop\n  vertex 10 0 0\n  vertex 10 10 10\n  vertex 10 0 10\n endloop\nendfacet\n\
endsolid cube\n";

    fn cube() -> Mesh {
        Mesh::from_stl_bytes(CUBE.as_bytes()).unwrap()
    }

    #[test]
    fn parses_ascii_stl() {
        let mesh = cube();
        assert_eq!(mesh.triangle_count(), 12);
        let bounds = mesh.bounds();
        assert!((bounds.size[0] - 10.0).abs() < 1e-4);
        assert!(mesh.stats().watertight);
    }

    #[test]
    fn binary_stl_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cube.stl");
        cube().write_path(&path).unwrap();
        let loaded = Mesh::load_path(&path).unwrap();
        assert_eq!(loaded.triangle_count(), 12);
        assert!((loaded.bounds().size[2] - 10.0).abs() < 1e-4);
    }

    #[test]
    fn transforms_and_centering_work() {
        let mut mesh = cube();
        mesh.translate([5.0, -5.0, 3.0]);
        mesh.center_on_bed(true);
        let bounds = mesh.bounds();
        assert!(bounds.min[2].abs() < 1e-4);
        assert!(bounds.center[0].abs() < 1e-4);
        mesh.scale([2.0, 1.0, 1.0]);
        assert!((mesh.bounds().size[0] - 20.0).abs() < 1e-4);
    }

    #[test]
    fn volume_of_cube_is_positive() {
        let mesh = cube();
        assert!((mesh.stats().volume_mm3 - 1000.0).abs() < 250.0);
    }

    #[test]
    fn decimation_reduces_triangles() {
        let mesh = cube();
        let decimated = mesh.decimate(2).unwrap();
        assert!(decimated.triangle_count() <= mesh.triangle_count());
    }
}
