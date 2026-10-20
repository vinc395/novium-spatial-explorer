//! `xenium-web` command line.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use xenium_core::registration::AlignmentOutputUnits;
use xenium_index::{GridInput, ImageInput, IngestConfig, RegistrationInput};

mod certs;
use certs::write_private;

#[derive(Parser)]
#[command(
    name = "xenium-web",
    version,
    about = "NOVIUM Spatial Explorer: Xenium + H&E viewer (index builder and server)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Units {
    /// Matrix maps image px -> Xenium morphology px (10x Xenium Explorer convention).
    MorphologyPx,
    /// Matrix maps image px -> Xenium µm.
    Um,
}

#[derive(Clone, Copy, ValueEnum)]
enum Variant {
    Rotated,
    Mirrored,
}

// Parsed once per process; boxing the large `Ingest` variant would only add noise.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Cmd {
    /// Write a deterministic synthetic Xenium-like section (not biological data).
    Synth {
        #[arg(long)]
        out: PathBuf,
        #[arg(long, value_enum, default_value = "rotated")]
        variant: Variant,
        #[arg(long, default_value_t = 40)]
        cols: u32,
        #[arg(long, default_value_t = 25)]
        rows: u32,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long, default_value_t = 100.0)]
        transcripts_per_cell: f64,
        /// Mimic older XOA outputs (binary feature_name, no codeword_category/is_gene).
        #[arg(long)]
        legacy_columns: bool,
        /// Also write xenium/morphology_focus/ (four synthetic JPEG 2000 channels).
        #[arg(long)]
        morphology: bool,
    },
    /// Print which Xenium files and columns are present (schemas and counts only; no data values).
    Inspect {
        #[arg(long)]
        xenium: Option<PathBuf>,
        /// Also probe a TIFF image (IFDs, compression, pixel size).
        #[arg(long)]
        image: Vec<PathBuf>,
    },
    /// Build, validate and atomically publish an index for one section.
    Ingest {
        #[arg(long)]
        section_id: String,
        #[arg(long)]
        name: Option<String>,
        /// Xenium output folder (read-only).
        #[arg(long)]
        xenium: PathBuf,
        /// Index root (sections are published as <root>/<section-id>).
        #[arg(long)]
        index_root: PathBuf,
        /// Registered/native H&E image (TIFF / OME-TIFF).
        #[arg(long)]
        he: Option<PathBuf>,
        /// registration.json for --he (see docs/DATA_CONTRACT.md).
        #[arg(long, conflicts_with = "he_alignment_csv")]
        he_registration: Option<PathBuf>,
        /// 10x Xenium Explorer *_imagealignment.csv for --he.
        #[arg(long)]
        he_alignment_csv: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "morphology-px")]
        he_alignment_units: Units,
        /// The H&E is already on the Xenium µm grid (origin aligned, no rotation) at this pixel size.
        #[arg(long, conflicts_with_all = ["he_registration", "he_alignment_csv"])]
        he_already_aligned_pixel_size: Option<f64>,
        /// Use specific TIFF IFDs: one index, or three (R,G,B) single-channel IFDs.
        #[arg(long, value_delimiter = ',')]
        he_ifds: Option<Vec<usize>>,
        /// Per-cell annotation table(s) (.csv/.tsv[.gz]/.parquet, one row per cell).
        #[arg(long)]
        annotations: Vec<PathBuf>,
        #[arg(long, default_value = "cell_id")]
        annotation_id_column: String,
        /// Columns to import from the annotation tables, e.g.
        /// `final_cell_type,major_cell_type,global_cluster:cat,N0*_mass` (`*` glob;
        /// `:cat` / `:num` force a type). Each column is taken from the first table
        /// that has it. Default: all columns.
        #[arg(long)]
        annotation_columns: Option<String>,
        /// Long-format precomputed LR score table(s): cell_id, pair_id, [role], score, ...
        #[arg(long)]
        lr_scores: Vec<PathBuf>,
        /// Column bundle(s) (`*.json` + cells + float32 data), e.g. from scripts/cytosignal_to_xw.R.
        #[arg(long)]
        columns: Vec<PathBuf>,
        /// Segmentation-free grid as ID=PATH (x/y columns are grid-cell centres in µm).
        #[arg(long)]
        grid: Vec<String>,
        #[arg(long, default_value = "x_um")]
        grid_x_col: String,
        #[arg(long, default_value = "y_um")]
        grid_y_col: String,
        #[arg(long)]
        grid_spacing_um: Option<f64>,
        /// Columns for one grid as ID=COLS (same syntax as --annotation-columns). Repeatable.
        #[arg(long)]
        grid_columns: Vec<String>,
        #[arg(long, default_value_t = 256.0)]
        tile_size_um: f64,
        #[arg(long, default_value_t = 85)]
        jpeg_quality: u8,
        #[arg(long)]
        skip_transcripts: bool,
        #[arg(long, default_value_t = 2)]
        keep_versions: usize,
        /// Xenium morphology_focus/ directory: each *.ome.tif channel (DAPI, boundary,
        /// interior stains; JPEG 2000) becomes a fluorescence image layer.
        #[arg(long)]
        morphology: Option<PathBuf>,
        /// Worker threads for image encoding (default: all cores).
        #[arg(long)]
        threads: Option<usize>,
    },
    /// Registration QC: H&E darkness at Xenium nucleus centres vs shifted positions
    /// (numbers only; the image is never displayed).
    QcRegistration {
        #[arg(long)]
        index: PathBuf,
        #[arg(long)]
        image: Option<String>,
        #[arg(long, default_value_t = 20000)]
        max_nuclei: usize,
        #[arg(long, default_value_t = 20.0)]
        radius_um: f64,
        #[arg(long, default_value_t = 1.0)]
        step_um: f64,
        /// Also report the peak per block of an N x N partition of the section.
        #[arg(long, default_value_t = 3)]
        blocks: usize,
    },
    /// Score gene signatures with UCell (same algorithm as the server and R UCell)
    /// and write the per-cell (or per-bin) scores without a server.
    Ucell {
        /// Section index directory (e.g. <index-root>/<section>).
        #[arg(long)]
        index: PathBuf,
        /// Signatures: JSON object {"name": ["GENE", "GENE-", ...]} or a .gmt file.
        #[arg(long)]
        signatures: PathBuf,
        #[arg(long, default_value_t = xenium_score::ucell::DEFAULT_MAX_RANK)]
        max_rank: u32,
        #[arg(long, default_value_t = 1.0)]
        w_neg: f64,
        /// Genes absent from the panel: impute (counted at maxRank, R default) or skip.
        #[arg(long, default_value = "impute")]
        missing_genes: String,
        /// Score square bins of pooled transcripts of this size (µm) instead of cells.
        #[arg(long)]
        bin_um: Option<f64>,
        #[arg(long, default_value_t = 10)]
        min_transcripts: u32,
        /// Output: .parquet, or .tsv (unit id + %.17g scores, NaN kept).
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 0)]
        threads: usize,
    },
    /// Create a local certificate authority (ca.crt + ca.key) for LAN sharing.
    /// Colleagues install ca.crt once as trusted; it can only vouch for the
    /// --permit subnets/names (X.509 name constraints).
    GenCa {
        #[arg(long)]
        out_dir: PathBuf,
        /// Comma-separated subnets (CIDR), IPs and host names, e.g.
        /// 192.168.10.0/23,myhost,localhost,127.0.0.1
        #[arg(long, value_delimiter = ',', required = true)]
        permit: Vec<String>,
        #[arg(long, default_value = "xenium-web local CA")]
        name: String,
        #[arg(long, default_value_t = 3650)]
        days: u32,
        /// Replace an existing CA (everyone must then install the new ca.crt).
        #[arg(long)]
        force: bool,
    },
    /// Create the HTTPS server certificate (cert.pem + key.pem) for the given host
    /// names / IP addresses: signed by the CA in --ca-dir, or self-signed without it.
    GenCert {
        /// Comma-separated names and IPs, e.g. 192.168.11.21,myhost,localhost
        #[arg(long, value_delimiter = ',', required = true)]
        hosts: Vec<String>,
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long)]
        ca_dir: Option<PathBuf>,
        /// Validity (at most 825 days, Apple's limit for trusted server certificates).
        #[arg(long, default_value_t = 397)]
        days: u32,
        /// Keep an existing certificate that is issued by the CA, covers all hosts and
        /// is valid for 30+ more days.
        #[arg(long)]
        if_needed: bool,
    },
    /// Print a certificate's SHA-256 and SHA-1 fingerprints.
    Fingerprint { cert: PathBuf },
    /// Validate a published index (exit code 1 on errors).
    Validate {
        #[arg(long)]
        index: PathBuf,
    },
    /// Serve indexes and the web UI.
    Serve {
        #[arg(long)]
        index_root: PathBuf,
        /// Built web UI (web/dist). Omit when using the Vite dev server.
        #[arg(long)]
        static_dir: Option<PathBuf>,
        #[arg(long, default_value = "127.0.0.1")]
        host: IpAddr,
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// Required to bind a non-loopback address (needs --access-token-file and TLS).
        #[arg(long)]
        allow_remote: bool,
        /// Require this access token (file is created with a random token if missing).
        #[arg(long)]
        access_token_file: Option<PathBuf>,
        /// HTTPS certificate and key (PEM), e.g. from `xenium-web gen-cert`.
        #[arg(long, requires = "tls_key")]
        tls_cert: Option<PathBuf>,
        #[arg(long, requires = "tls_cert")]
        tls_key: Option<PathBuf>,
        /// Allow a token over plain HTTP on the network (token visible to network observers).
        #[arg(long)]
        allow_plain_http: bool,
        #[arg(long, default_value_t = 512)]
        cache_mb: usize,
        /// Writable directory for gene signatures and cached UCell results
        /// [default: <index-root>/.xenium-web-state].
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Threads for UCell scoring jobs (0 = half the cores, leaving the rest
        /// for tile requests).
        #[arg(long, default_value_t = 0)]
        score_threads: usize,
        /// File holding a password that also opens the sign-in page (in addition to
        /// the private token link). Read when the server starts.
        #[arg(long, requires = "access_token_file")]
        access_password_file: Option<PathBuf>,
        /// CSV with per-sample information to show in the Index panel (e.g. age, sex,
        /// timepoint). Read at start-up; only --sample-metadata-columns are kept.
        #[arg(long, requires = "sample_metadata_columns")]
        sample_metadata: Option<PathBuf>,
        /// Column of the CSV holding the section (sample) ids.
        #[arg(long, default_value = "sample_id")]
        sample_metadata_id: String,
        /// Comma-separated columns to show, in this order, e.g. patient,Sex,Age,timepoint,treatment.
        #[arg(long, value_delimiter = ',')]
        sample_metadata_columns: Vec<String>,
        /// Per-person access instead of a token link or shared password: an email list
        /// with individual passwords (manage it with `xenium-web users`). Changes to the
        /// file apply at once; removing someone ends their sessions.
        #[arg(long, conflicts_with_all = ["access_token_file", "access_password_file"])]
        access_users_file: Option<PathBuf>,
        /// This listener is reached only through a local Cloudflare tunnel (HTTPS at
        /// Cloudflare): cookies are marked Secure and wrong sign-ins are limited per
        /// visitor (CF-Connecting-IP). Only on a loopback address.
        #[arg(long)]
        behind_cloudflare: bool,
    },
    /// Manage the email list for `serve --access-users-file` (each person gets their own
    /// generated password; only its hash is stored).
    Users {
        /// The access list file (created with owner-only permissions).
        #[arg(long)]
        file: PathBuf,
        #[command(subcommand)]
        action: UsersCmd,
    },
}

#[derive(Subcommand)]
enum UsersCmd {
    /// Add someone (or give them a new password). Without --password-file a password is
    /// generated and printed once as "email<TAB>password".
    Add {
        email: String,
        /// Give them the password in this file (e.g. a master password) instead.
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    /// Give everyone on the list the password in --password-file.
    ResetAll {
        #[arg(long)]
        password_file: PathBuf,
    },
    /// Remove someone; their sessions end at their next request.
    Remove { email: String },
    /// List the emails on the access list.
    List,
}

/// First line of a password file, without the line break.
fn read_password_file(p: &std::path::Path) -> Result<String> {
    let text = std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?;
    Ok(text.lines().next().unwrap_or("").to_string())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // OpenJPEG reports every decoded tile at info level.
                .unwrap_or_else(|_| "info,tower_http=warn,jpeg2k=warn".into()),
        )
        .with_target(false)
        .init();
    match Cli::parse().cmd {
        Cmd::Synth {
            out,
            variant,
            cols,
            rows,
            seed,
            transcripts_per_cell,
            legacy_columns,
            morphology,
        } => synth(
            &out,
            variant,
            (cols, rows),
            seed,
            transcripts_per_cell,
            legacy_columns,
            morphology,
        ),
        Cmd::Inspect { xenium, image } => inspect(xenium.as_deref(), &image),
        Cmd::Ingest {
            section_id,
            name,
            xenium,
            index_root,
            he,
            he_registration,
            he_alignment_csv,
            he_alignment_units,
            he_already_aligned_pixel_size,
            he_ifds,
            annotations,
            annotation_id_column,
            annotation_columns,
            lr_scores,
            columns,
            grid,
            grid_x_col,
            grid_y_col,
            grid_spacing_um,
            grid_columns,
            tile_size_um,
            jpeg_quality,
            skip_transcripts,
            keep_versions,
            morphology,
            threads,
        } => {
            if let Some(t) = threads {
                rayon::ThreadPoolBuilder::new()
                    .num_threads(t)
                    .build_global()?;
            }
            let mut cfg = IngestConfig::new(&section_id, &xenium, &index_root);
            cfg.display_name = name;
            cfg.tile_size_um = tile_size_um;
            cfg.jpeg_quality = jpeg_quality.clamp(30, 100);
            cfg.annotations = annotations;
            cfg.annotation_id_column = annotation_id_column;
            cfg.annotation_columns = annotation_columns
                .as_deref()
                .map(xenium_index::tables_ingest::ColumnSpec::parse_list)
                .transpose()?;
            let mut grid_cols = std::collections::HashMap::new();
            for gc in &grid_columns {
                let (id, cols) = gc
                    .split_once('=')
                    .context("--grid-columns expects ID=COLS")?;
                grid_cols.insert(
                    id.to_string(),
                    xenium_index::tables_ingest::ColumnSpec::parse_list(cols)?,
                );
            }
            cfg.lr_scores = lr_scores;
            cfg.column_bundles = columns;
            cfg.skip_transcripts = skip_transcripts;
            cfg.keep_versions = keep_versions;
            cfg.morphology = morphology;
            if let Some(path) = he {
                let registration = match (he_registration, he_alignment_csv, he_already_aligned_pixel_size) {
                    (Some(p), None, None) => RegistrationInput::Json(p),
                    (None, Some(p), None) => RegistrationInput::AlignmentCsv {
                        path: p,
                        units: match he_alignment_units {
                            Units::MorphologyPx => AlignmentOutputUnits::XeniumMorphologyPx,
                            Units::Um => AlignmentOutputUnits::Um,
                        },
                    },
                    (None, None, Some(ps)) => RegistrationInput::Spec(
                        xenium_core::registration::RegistrationSpec::AlreadyAligned { pixel_size_um: ps },
                    ),
                    _ => bail!(
                        "--he needs exactly one of --he-registration, --he-alignment-csv or --he-already-aligned-pixel-size; \
                         the transform is never guessed"
                    ),
                };
                cfg.images.push(ImageInput {
                    id: "he".into(),
                    label: "H&E".into(),
                    path,
                    registration,
                    ifds: he_ifds,
                });
            } else if he_registration.is_some() || he_alignment_csv.is_some() {
                bail!("a registration was given without --he");
            }
            for g in grid {
                let (id, path) = g.split_once('=').context("--grid expects ID=PATH")?;
                cfg.grids.push(GridInput {
                    id: id.to_string(),
                    label: id.to_string(),
                    path: PathBuf::from(path),
                    x_col: grid_x_col.clone(),
                    y_col: grid_y_col.clone(),
                    spacing_um: grid_spacing_um,
                    columns: grid_cols.remove(id),
                });
            }
            if let Some(id) = grid_cols.keys().next() {
                bail!("--grid-columns given for unknown grid {id:?}");
            }
            let t = std::time::Instant::now();
            let dir = xenium_index::ingest(&cfg)?;
            println!(
                "published {} -> {} in {:.1?}",
                section_id,
                dir.display(),
                t.elapsed()
            );
            let report: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.join("import_report.json"))?)?;
            if let Some(w) = report["warnings"].as_array().filter(|w| !w.is_empty()) {
                println!("warnings:");
                for x in w {
                    println!("  - {}", x.as_str().unwrap_or_default());
                }
            }
            Ok(())
        }
        Cmd::QcRegistration {
            index,
            image,
            max_nuclei,
            radius_um,
            step_um,
            blocks,
        } => {
            let store = xenium_index::SectionStore::open(&index)?;
            let r = xenium_index::qc::registration_qc(
                &store,
                image.as_deref(),
                max_nuclei,
                radius_um,
                step_um,
                blocks,
            )?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            Ok(())
        }
        Cmd::Ucell {
            index,
            signatures,
            max_rank,
            w_neg,
            missing_genes,
            bin_um,
            min_transcripts,
            out,
            threads,
        } => run_ucell(
            &index,
            &signatures,
            max_rank,
            w_neg,
            &missing_genes,
            bin_um,
            min_transcripts,
            &out,
            threads,
        ),
        Cmd::Validate { index } => {
            let r = xenium_index::validate::validate_dir(&index)?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            if !r.errors.is_empty() {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::GenCa {
            out_dir,
            permit,
            name,
            days,
            force,
        } => {
            let fp = certs::gen_ca(&out_dir, &name, &permit, days, force)?;
            println!(
                "wrote {} (share this) and {} (keep private)",
                out_dir.join(certs::CA_CERT).display(),
                out_dir.join(certs::CA_KEY).display()
            );
            println!("may sign for: {}", permit.join(", "));
            println!("SHA-256 fingerprint: {}", fp.sha256);
            println!("SHA-1 thumbprint:    {}", fp.sha1);
            Ok(())
        }
        Cmd::GenCert {
            hosts,
            out_dir,
            ca_dir,
            days,
            if_needed,
        } => {
            let r = certs::gen_server_cert(&out_dir, &hosts, ca_dir.as_deref(), days, if_needed)?;
            if r.issued {
                println!(
                    "wrote {} and {} ({}; {} for {}, {days} days)",
                    r.cert.display(),
                    r.key.display(),
                    r.reason,
                    if ca_dir.is_some() {
                        "CA-signed"
                    } else {
                        "self-signed"
                    },
                    hosts.join(", ")
                );
            } else {
                println!("kept {} ({})", r.cert.display(), r.reason);
            }
            Ok(())
        }
        Cmd::Fingerprint { cert } => {
            let pem = std::fs::read_to_string(&cert)
                .with_context(|| format!("read {}", cert.display()))?;
            let der = ::pem::parse(pem).map_err(|e| anyhow::anyhow!("{}: {e}", cert.display()))?;
            let fp = certs::fingerprints(der.contents());
            println!("SHA-256: {}\nSHA-1:   {}", fp.sha256, fp.sha1);
            Ok(())
        }
        Cmd::Users { file, action } => {
            match action {
                UsersCmd::Add {
                    email,
                    password_file: Some(p),
                } => {
                    let email = xenium_server::normalize_email(&email)?;
                    let pw = read_password_file(&p)?;
                    xenium_server::people_add_with_password(&file, &email, &pw)?;
                    eprintln!(
                        "added {email} to {} with the password in {}",
                        file.display(),
                        p.display()
                    );
                }
                UsersCmd::Add {
                    email,
                    password_file: None,
                } => {
                    let email = xenium_server::normalize_email(&email)?;
                    let password = xenium_server::people_add(&file, &email)?;
                    eprintln!(
                        "added {email} to {} (the password is printed once; only its hash is kept)",
                        file.display()
                    );
                    println!("{email}\t{password}");
                }
                UsersCmd::ResetAll { password_file } => {
                    let pw = read_password_file(&password_file)?;
                    let n = xenium_server::people_reset_all(&file, &pw)?;
                    eprintln!(
                        "gave all {n} people on {} the password in {}",
                        file.display(),
                        password_file.display()
                    );
                }
                UsersCmd::Remove { email } => {
                    if !xenium_server::people_remove(&file, &email)? {
                        bail!("{email} is not on {}", file.display());
                    }
                    eprintln!("removed {email}; their sessions end at their next request");
                }
                UsersCmd::List => {
                    for e in xenium_server::people_list(&file)? {
                        println!("{e}");
                    }
                }
            }
            Ok(())
        }
        Cmd::Serve {
            index_root,
            static_dir,
            host,
            port,
            allow_remote,
            access_token_file,
            tls_cert,
            tls_key,
            allow_plain_http,
            cache_mb,
            state_dir,
            score_threads,
            access_password_file,
            sample_metadata,
            sample_metadata_id,
            sample_metadata_columns,
            access_users_file,
            behind_cloudflare,
        } => {
            if !host.is_loopback() && !allow_remote {
                bail!("refusing to bind {host}: use 127.0.0.1 with an SSH tunnel, or pass --allow-remote with --access-token-file and TLS");
            }
            if !host.is_loopback() && access_token_file.is_none() && access_users_file.is_none() {
                bail!(
                    "refusing to serve {host} without --access-token-file or --access-users-file"
                );
            }
            if behind_cloudflare && !host.is_loopback() {
                bail!("--behind-cloudflare trusts the visitor address sent by the tunnel, so it needs a loopback --host (e.g. 127.0.0.1)");
            }
            if behind_cloudflare && access_token_file.is_none() && access_users_file.is_none() {
                bail!("refusing to serve through a public tunnel without --access-users-file or --access-token-file");
            }
            if !host.is_loopback() && tls_cert.is_none() && !allow_plain_http {
                bail!("refusing to send an access token over plain HTTP on {host}: pass --tls-cert/--tls-key (see `xenium-web gen-cert`)");
            }
            let token = match &access_token_file {
                None => None,
                Some(p) if p.exists() => Some(std::fs::read_to_string(p)?.trim().to_string()),
                Some(p) => {
                    let t = xenium_server::generate_token()?;
                    if let Some(d) = p.parent() {
                        std::fs::create_dir_all(d)?;
                    }
                    write_private(p, t.as_bytes())?;
                    println!("created access token file {}", p.display());
                    Some(t)
                }
            };
            if let Some(d) = &static_dir {
                if !d.join("index.html").is_file() {
                    bail!(
                        "{} has no index.html (run `npm run build` in web/)",
                        d.display()
                    );
                }
            }
            let sample_info = match &sample_metadata {
                Some(p) => {
                    let info = xenium_server::SampleInfo::load(
                        p,
                        &sample_metadata_id,
                        &sample_metadata_columns,
                    )?;
                    let sections: Vec<String> = std::fs::read_dir(&index_root)?
                        .flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .filter(|n| {
                            !n.starts_with('.')
                                && index_root.join(n).join("manifest.json").is_file()
                        })
                        .collect();
                    let missing: Vec<&String> = sections.iter().filter(|s| !info.has(s)).collect();
                    println!(
                        "sample information: {} for {} of {} sections{}",
                        info.columns.join(", "),
                        sections.len() - missing.len(),
                        sections.len(),
                        if missing.is_empty() {
                            String::new()
                        } else {
                            format!(
                                " (none for {})",
                                missing
                                    .iter()
                                    .map(|s| s.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        }
                    );
                    if info.duplicate_ids > 0 {
                        println!(
                            "  note: {} rows repeat an id; the first row is used",
                            info.duplicate_ids
                        );
                    }
                    Some(std::sync::Arc::new(info))
                }
                None => None,
            };
            let cfg = xenium_server::ServerConfig {
                index_root,
                static_dir: static_dir.clone(),
                cache_bytes: cache_mb << 20,
                max_blocking: 32,
                state_dir,
                score_threads,
                sample_info,
            };
            let state = xenium_server::AppState::new(&cfg)?;
            let mut app = xenium_server::router(state, static_dir.as_deref());
            let password = match &access_password_file {
                Some(p) => Some(
                    std::fs::read_to_string(p)
                        .with_context(|| format!("read {}", p.display()))?
                        .trim()
                        .to_string(),
                ),
                None => None,
            };
            let auth = match (&access_users_file, &token) {
                (Some(f), _) => Some(xenium_server::Auth::people(f, tls_cert.is_some())?),
                (None, Some(t)) => {
                    let mut auth = xenium_server::Auth::new(t.clone(), tls_cert.is_some())?;
                    if let Some(pw) = &password {
                        auth = auth.with_password(pw)?;
                    }
                    Some(auth)
                }
                (None, None) => None,
            };
            let people = auth.as_ref().and_then(|a| a.people_count());
            if let Some(mut auth) = auth {
                if behind_cloudflare {
                    auth = auth.behind_cloudflare();
                }
                app = xenium_server::with_auth(app, auth);
            }
            // Disk reads are capped at 32 (`max_blocking`); this bounds everything
            // else on the blocking pool (static files, scoring, password hashing).
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .max_blocking_threads(64)
                .build()?;
            rt.block_on(async move {
                let addr = SocketAddr::new(host, port);
                let scheme = if tls_cert.is_some() { "https" } else { "http" };
                println!("NOVIUM Spatial Explorer (xenium-web) serving on {scheme}://{addr}");
                if let Some(t) = &token {
                    // Only to a terminal: a service's output is kept in the system journal.
                    if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                        println!("share this link (keep it private): {scheme}://{addr}/?token={t}");
                    } else {
                        println!("access link not printed (output is not a terminal); the token is in the access token file");
                    }
                }
                if password.is_some() {
                    println!("or open {scheme}://{addr}/ and sign in with the password");
                }
                if let Some(n) = people {
                    println!("sign-in: email and password, for the {n} people on the access list");
                }
                match (&tls_cert, &tls_key) {
                    (Some(c), Some(k)) => xenium_server::serve_tls(addr, app, c, k).await,
                    _ => {
                        let listener = tokio::net::TcpListener::bind(addr)
                            .await
                            .with_context(|| format!("bind {addr}"))?;
                        xenium_server::serve(listener, app).await
                    }
                }
            })
        }
    }
}

fn synth(
    out: &Path,
    variant: Variant,
    (cols, rows): (u32, u32),
    seed: u64,
    tpc: f64,
    legacy: bool,
    morphology: bool,
) -> Result<()> {
    let opts = xenium_io::synth::SynthOptions {
        seed,
        cols,
        rows,
        transcripts_per_cell: tpc,
        legacy_columns: legacy,
        morphology,
        he: match variant {
            Variant::Rotated => xenium_io::synth::HeVariant::Rotated,
            Variant::Mirrored => xenium_io::synth::HeVariant::Mirrored,
        },
        ..Default::default()
    };
    let o = xenium_io::synth::generate(out, &opts)?;
    println!(
        "synthetic section written to {} (NOT biological data)",
        out.display()
    );
    println!("{}", serde_json::to_string_pretty(&o.truth)?);
    Ok(())
}

fn inspect(xenium: Option<&Path>, images: &[PathBuf]) -> Result<()> {
    if let Some(x) = xenium {
        let files = xenium_io::XeniumFiles::discover(x)?;
        println!("{}", serde_json::to_string_pretty(&files)?);
        if let Some(p) = &files.experiment {
            println!(
                "experiment: {}",
                serde_json::to_string_pretty(&xenium_io::experiment::read_experiment(p)?)?
            );
        }
        for p in [
            &files.cells,
            &files.cell_boundaries,
            &files.nucleus_boundaries,
            &files.transcripts,
        ]
        .into_iter()
        .flatten()
        {
            let schema = xenium_io::columns::parquet_schema(p)?;
            let (_, rows, _) = xenium_io::columns::open_projected(p, &[])?;
            println!(
                "\n{} ({rows} rows)",
                p.file_name().unwrap().to_string_lossy()
            );
            for (n, t) in schema {
                println!("  {n}: {t}");
            }
        }
        if let Some(p) = &files.transcripts {
            println!(
                "\ntranscript columns: {:?}",
                xenium_io::transcripts::detect_columns(p)?
            );
        }
    }
    for img in images {
        println!(
            "\n{}: {}",
            img.display(),
            serde_json::to_string_pretty(&xenium_io::tiff_image::probe(img)?)?
        );
    }
    Ok(())
}

/// Signatures from a JSON object (`{"name": [genes]}`, order kept) or a GMT file.
fn read_signatures(path: &Path) -> Result<Vec<(String, Vec<String>)>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gmt"))
    {
        return Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let mut it = l.split('\t');
                let name = it.next().unwrap_or("").trim().to_string();
                let genes = it
                    .skip(1)
                    .map(|g| g.trim().to_string())
                    .filter(|g| !g.is_empty())
                    .collect();
                (name, genes)
            })
            .collect());
    }
    let v: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&text).context("signatures JSON must be an object")?;
    v.into_iter()
        .map(|(k, g)| {
            Ok((
                k,
                serde_json::from_value::<Vec<String>>(g).context("gene list")?,
            ))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn run_ucell(
    index: &Path,
    signatures: &Path,
    max_rank: u32,
    w_neg: f64,
    missing_genes: &str,
    bin_um: Option<f64>,
    min_transcripts: u32,
    out: &Path,
    threads: usize,
) -> Result<()> {
    use xenium_score::cache::{Level, ResultMeta, SignatureSnapshot, Values};
    use xenium_score::section::{coverage, score_bins, score_cells, stats, SectionPanel};
    use xenium_score::ucell::{MissingGenes, UcellParams, ALGORITHM};
    let missing = match missing_genes {
        "impute" => MissingGenes::Impute,
        "skip" => MissingGenes::Skip,
        o => bail!("--missing-genes must be impute or skip, not {o:?}"),
    };
    let params = UcellParams {
        max_rank,
        w_neg,
        missing_genes: missing,
    };
    let store = xenium_index::SectionStore::open(index)?;
    let panel = SectionPanel::from_manifest(&store.manifest)?;
    let sigs = read_signatures(signatures)?;
    for (name, genes) in &sigs {
        let c = coverage(&store, &panel, genes, &params);
        eprintln!(
            "{name}: {}/{} positive and {}/{} negative genes in the panel ({} imputed){}",
            c.positive_found,
            c.positive_total,
            c.negative_found,
            c.negative_total,
            c.imputed,
            c.error
                .map(|e| format!(" - ERROR: {e}"))
                .unwrap_or_default()
        );
    }
    let genes: Vec<Vec<String>> = sigs.iter().map(|s| s.1.clone()).collect();
    let scorer = panel.scorer(&genes, &params)?;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()?;
    let progress = xenium_score::progress::Progress::new();
    let t0 = std::time::Instant::now();
    let level = match bin_um {
        None => Level::Cell,
        Some(b) => Level::Bin {
            bin_um: b,
            min_transcripts,
        },
    };
    let (cols, n_tx, grid) = pool.install(|| -> Result<_> {
        Ok(match &level {
            Level::Cell => (score_cells(&store, &scorer, &progress)?, None, None),
            Level::Bin {
                bin_um,
                min_transcripts,
            } => {
                let b = score_bins(
                    &store,
                    &panel,
                    &scorer,
                    *bin_um,
                    *min_transcripts,
                    &progress,
                )?;
                (b.scores, Some(b.n_transcripts), Some(b.grid))
            }
        })
    })?;
    let ms = t0.elapsed().as_millis() as u64;
    eprintln!(
        "UCell ({ALGORITHM}): {} signatures x {} {} in {ms} ms; panel {} genes, maxRank {}",
        sigs.len(),
        cols.first().map(|c| c.len()).unwrap_or(0),
        if bin_um.is_some() { "bins" } else { "cells" },
        panel_genes(&panel),
        scorer.max_rank
    );
    let is_tsv = out
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("tsv"));
    if is_tsv {
        use std::io::Write;
        let mut w = std::io::BufWriter::new(std::fs::File::create(out)?);
        writeln!(
            w,
            "unit\t{}",
            sigs.iter()
                .map(|s| s.0.as_str())
                .collect::<Vec<_>>()
                .join("\t")
        )?;
        for u in 0..cols.first().map(|c| c.len()).unwrap_or(0) {
            let id = match &grid {
                None => store
                    .cell_dictionary()
                    .id_of(u as u32 + 1)
                    .unwrap_or("")
                    .to_string(),
                Some(g) => format!("bin_{}_{}", u % g.width as usize, u / g.width as usize),
            };
            let vals: Vec<String> = cols.iter().map(|c| fmt17(c[u])).collect();
            writeln!(w, "{id}\t{}", vals.join("\t"))?;
        }
        w.flush()?;
    } else {
        let results: Vec<(ResultMeta, Values)> = sigs
            .iter()
            .zip(cols)
            .map(|((name, genes), col)| {
                let meta = ResultMeta {
                    key: xenium_score::cache::result_key(genes, &params, &level),
                    section_id: store.manifest.section_id.clone(),
                    index_version: index
                        .canonicalize()
                        .ok()
                        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
                        .unwrap_or_default(),
                    signature: SignatureSnapshot {
                        id: None,
                        name: name.clone(),
                        genes: genes.clone(),
                    },
                    params: params.clone(),
                    level: level.clone(),
                    algorithm: ALGORITHM.into(),
                    n_units: col.len(),
                    grid,
                    coverage: coverage(&store, &panel, genes, &params),
                    stats: stats(&col),
                    computed_unix: 0,
                    duration_ms: ms,
                };
                (
                    meta,
                    Values {
                        scores: col,
                        n_transcripts: n_tx.clone(),
                    },
                )
            })
            .collect();
        let bytes = match level {
            Level::Cell => xenium_score::export::cells_parquet(&store, &results)?,
            Level::Bin { .. } => {
                xenium_score::export::bins_parquet(&store.manifest.section_id, &results)?
            }
        };
        std::fs::write(out, bytes)?;
    }
    eprintln!("wrote {}", out.display());
    Ok(())
}

fn panel_genes(p: &xenium_score::section::SectionPanel) -> usize {
    use xenium_score::ucell::Panel;
    p.n_genes()
}

/// R's `sprintf("%.17g", x)` for finite values; NaN as "NaN".
fn fmt17(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    // Shortest round-trip representation; parses back to the identical f64.
    format!("{x:?}")
}
