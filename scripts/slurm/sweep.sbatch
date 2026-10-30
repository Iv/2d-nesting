#!/bin/bash
# Seed sweep: one nesting run per array task, each with its own seed.
#
#   sbatch --array=1-64 scripts/slurm/sweep.sbatch <input dir> [extra nest options...]
#
# Results: runs/sweep_<array job id>/seed_<task id>/; summarise with scripts/sweep_summary.py.
#SBATCH --job-name=nest-sweep
#SBATCH --partition=main
#SBATCH --cpus-per-task=3
#SBATCH --mem=2G
#SBATCH --time=02:00:00
#SBATCH --output=runs/slurm/%x_%A_%a.out

set -euo pipefail
input="$1"
shift
seed="${SLURM_ARRAY_TASK_ID:-1}"
out="runs/sweep_${SLURM_ARRAY_JOB_ID:-local}/seed_${seed}"
mkdir -p "$out"
# The worker count must match --cpus-per-task, and stay fixed for comparable runs.
./target/release/nest run "$input" -o "$out" -s "$seed" -w "${SLURM_CPUS_PER_TASK:-3}" "$@"
