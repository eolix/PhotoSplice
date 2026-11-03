//! The process-wide working CMYK (`set_working_cmyk`). In its own test binary: it changes what
//! every untagged CMYK conversion in the process uses, so one test drives it start to finish.

use std::sync::Arc;

use photosplice_cms::synth::{CmykParams, cmyk_profile};
use photosplice_cms::{Builtin, Profile};
use photosplice_color::convert::{CmykSpace, cmyk_to_rgb, set_working_cmyk, with_cmyk_space, working_cmyk, working_cmyk_profile};

fn press() -> Arc<Profile> {
    Arc::new(cmyk_profile(&CmykParams {
        description: "Working Test CMYK".into(),
        tvi: [0.26, 0.26, 0.26, 0.3],
        grid_a2b: 5,
        grid_b2a: 9,
        ..Default::default()
    }))
}

#[test]
fn untagged_cmyk_follows_the_working_profile() {
    let builtin_bytes = Builtin::CoatedCmyk.profile().to_bytes();
    let srgb_bytes = Builtin::Srgb.profile().to_bytes();
    // Until set: the built-in coated CMYK.
    assert!(working_cmyk().is_none());
    assert!(CmykSpace::for_profile(None).is_none());
    assert_eq!(working_cmyk_profile().content_hash(), Builtin::CoatedCmyk.profile().content_hash());

    let p = press();
    assert!(set_working_cmyk(Some(p.clone())));
    assert!(!set_working_cmyk(Some(p.clone())), "the same profile again changes nothing");
    let space = working_cmyk().unwrap();
    assert_eq!(space.id, p.content_hash());
    assert_eq!(working_cmyk_profile().description, "Working Test CMYK");
    // Untagged, or tagged with something that is not a CMYK profile: the working CMYK.
    for icc in [None, Some(&srgb_bytes), Some(&Arc::new(vec![1, 2, 3]))] {
        assert_eq!(CmykSpace::for_profile(icc).map(|s| s.id), Some(space.id));
    }
    // An embedded built-in profile stays the built-in one; another embedded profile is its own.
    assert!(CmykSpace::for_profile(Some(&builtin_bytes)).is_none());
    let other = cmyk_profile(&CmykParams { description: "Embedded".into(), grid_a2b: 3, grid_b2a: 3, ..Default::default() }).to_bytes();
    assert_eq!(CmykSpace::for_profile(Some(&other)).map(|s| s.id), Some(Profile::parse(&other).unwrap().content_hash()));

    // The pixels convert through it inside the scope, through the built-in one outside.
    let ink = [0.6, 0.2, 0.1, 0.05];
    let inside = with_cmyk_space(CmykSpace::for_profile(None).as_ref(), || cmyk_to_rgb(ink));
    let outside = cmyk_to_rgb(ink);
    assert!(inside.iter().zip(outside).any(|(a, b)| (a - b).abs() > 0.01), "{inside:?} {outside:?}");

    // The built-in profile, a non-CMYK profile or none: back to the built-in.
    for back in [Some(Arc::new(Builtin::CoatedCmyk.profile().clone())), Some(Arc::new(Builtin::Srgb.profile().clone())), None] {
        set_working_cmyk(Some(p.clone()));
        assert!(set_working_cmyk(back));
        assert!(working_cmyk().is_none() && CmykSpace::for_profile(None).is_none());
    }
}
