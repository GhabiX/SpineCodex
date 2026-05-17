use super::*;

fn next_u64(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    *seed
}

#[test]
fn segment_random_deterministic_canonical_cover_smoke() {
    let mut seed = 652_u64;
    for case_index in 0..64 {
        let raw_len = 32 + (next_u64(&mut seed) % 64);
        let first_start = next_u64(&mut seed) % 8;
        let first_width = 2 + (next_u64(&mut seed) % 8);
        let first_end = first_start + first_width;
        let second_start = first_end + (next_u64(&mut seed) % 8);
        let second_width = 2 + (next_u64(&mut seed) % 8);
        let second_end = (second_start + second_width).min(raw_len);
        if second_start >= second_end {
            continue;
        }

        let mut artifacts = SegmentArtifacts::new();
        artifacts.insert(
            format!("case-{case_index}-a"),
            RawSpan::new(first_start, first_end).expect("valid first span"),
        );
        artifacts.insert(
            format!("case-{case_index}-b"),
            RawSpan::new(second_start, second_end).expect("valid second span"),
        );
        let first_id = format!("case-{case_index}-a");
        let second_id = format!("case-{case_index}-b");

        let cover = canonical_cover(raw_len, [first_id.as_str(), second_id.as_str()], &artifacts)
            .expect("canonical cover");
        validate_cover(&cover, &artifacts).expect("valid cover");
        validate_future_live_boundaries(&cover, &artifacts, &[first_start, first_end, raw_len])
            .expect("valid live starts");
    }
}
