// TypeScript mirror of crates/xenium-core/src/manifest.rs (schema_version 2).

/** Index schema versions this viewer reads (2 added packed image tiles). */
export const SCHEMA_VERSION = 2;
export const MIN_SCHEMA_VERSION = 1;

export interface CategoryInfo { name: string; count: number }
export interface NumericStats {
  min: number | null; max: number | null; mean: number | null;
  p01: number | null; p99: number | null; n_valid: number; n_missing: number;
}
export interface FieldInfo {
  id: string;
  label: string;
  kind: "numeric" | "categorical";
  group: string;
  source_file: string;
  stats: NumericStats | null;
  categories: CategoryInfo[] | null;
  provenance: Record<string, unknown>;
}
export interface ShapeLayerInfo {
  polygons: number; vertices: number; query_margin_um: number;
  chunks: [number, number, number][]; skipped_polygons: number;
}
export interface GeneInfo { id: number; name: string; category: string; count: number; count_qv20: number }
export interface TranscriptsInfo {
  rows_read: number; indexed: number; skipped_invalid: number; unassigned: number; unknown_cell_ids: number;
  qv_scale: number; default_qv_min: number; z_available: boolean; genes: GeneInfo[];
  density_max_level: number; density_tile_bins: number;
}
export interface ResolvedRegistration {
  mode: string; matrix_xu_from_px: number[][]; approx_pixel_size_um: number; is_reflection: boolean; provenance: string;
}
/** Fluorescence morphology channel stored as an 8-bit pyramid (absent for H&E). */
export interface ChannelInfo { name: string; window: [number, number]; color: [number, number, number] }
export interface ImageInfo {
  id: string; label: string; kind: "rgb" | "gray"; width: number; height: number; tile_size: number; levels: number;
  format: string; registration: ResolvedRegistration; bounds_um: [number, number, number, number]; source_file: string; notes: string[];
  channel?: ChannelInfo | null;
  /** Tile storage on the server ("files" when absent); tile URLs are the same for both. */
  tile_layout?: "files" | "packed";
}
export interface GridInfo {
  id: string; label: string; origin_um: [number, number]; spacing_um: number; width: number; height: number;
  fields: FieldInfo[]; source_file: string; resampling: string;
}
export interface Manifest {
  schema_version: number;
  section_id: string;
  display_name: string;
  created_by: string;
  created_at_unix: number;
  world_frame: "xenium_um";
  bounds_um: [number, number, number, number];
  grid: { tile_size_um: number };
  xenium: { pixel_size_um: number | null; analysis_sw_version: string | null; panel_name: string | null; num_cells_reported: number | null };
  cells: { count: number; boundaries: ShapeLayerInfo | null; nuclei: ShapeLayerInfo | null; fields: FieldInfo[] } | null;
  transcripts: TranscriptsInfo | null;
  images: ImageInfo[];
  grids: GridInfo[];
  warnings: string[];
}
export interface SectionSummary {
  section_id: string; display_name?: string; version?: string; cells?: number; transcripts?: number;
  images?: { id: string; label: string }[]; error?: string;
  /** cell fields per group (e.g. lr_score); absent on older servers */
  field_groups?: Record<string, number>;
  /** per-sample information configured on the server: [column, value] pairs */
  sample?: [string, string][] | null;
}
export interface CellDetails {
  section_id: string; cell_key: number; cell_id: string; centroid_um: [number, number];
  fields: Record<string, number | string | null>;
  expression: {
    qv_min: number;
    total_genes: number;
    total_all_features: number;
    n_features: number;
    top: { gene_id: number; gene: string | null; count: number; is_gene: boolean }[];
  };
}

// ---- UCell (mirrors crates/xenium-score: cache.rs, section.rs, summary.rs) ----
export interface UcellParams { max_rank: number; w_neg: number; missing_genes: "impute" | "skip" }
export interface Signature { id: string; name: string; description: string; genes: string[]; created_unix: number; updated_unix: number }
export interface GeneCoverage {
  gene: string; sign: "+" | "-"; status: "found" | "missing" | "not_a_gene";
  cells_detected: number | null; fraction_detected: number | null; transcripts_qv20: number | null; suggestion: string | null;
}
export interface Coverage {
  panel_genes: number; max_rank_effective: number; n_entries: number;
  positive_found: number; positive_total: number; negative_found: number; negative_total: number;
  imputed: number; genes: GeneCoverage[]; error: string | null;
}
export interface BinGrid { origin_um: [number, number]; bin_um: number; width: number; height: number }
export interface ScoreStats {
  n_valid: number; n_missing: number; min: number | null; max: number | null; mean: number | null;
  p01: number | null; p50: number | null; p99: number | null;
}
export interface UcellResult {
  key: string; section_id: string; index_version: string;
  signature: { id: string | null; name: string; genes: string[] };
  params: UcellParams; level: "cell" | "bin"; bin_um?: number; min_transcripts?: number;
  algorithm: string; n_units: number; grid: BinGrid | null; coverage: Coverage; stats: ScoreStats;
  computed_unix: number; duration_ms: number;
}
export interface UcellJob {
  id: string; section_id: string; index_version: string;
  status: "queued" | "running" | "done" | "failed" | "cancelled";
  phase: string; done: number; total: number; elapsed_ms: number;
  level: { level: "cell" } | { level: "bin"; bin_um: number; min_transcripts: number };
  params: UcellParams; signatures: string[]; keys: string[]; error: string | null; results: UcellResult[] | null;
}
export interface GroupStats {
  name: string; n: number; mean: number; sd: number; min: number; p05: number; q1: number; median: number; q3: number; p95: number; max: number;
}
export interface UcellSummary {
  group_by: string; join: string; groups: GroupStats[]; n_unassigned: number; n_no_score: number; overall: GroupStats | null;
}

/** Menu label: the section name, marked when it carries LR scores. */
export function sectionLabel(s: SectionSummary): string {
  return `${s.display_name ?? s.section_id}${s.field_groups?.lr_score ? " · LR" : ""}`;
}
