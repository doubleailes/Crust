## 1. The fix

- [x] 1.1 `LightKind::found_along` answers for a dome (its `escaped` radiance and density, at infinity).
- [x] 1.2 `LightList::set_links` and the importer stop forcing a restricted dome to NEE alone; `CRUST_LINK_TWIN=0` keeps the old estimator for every restricted light.
- [x] 1.3 Comments and docs that called a restricted dome NEE-only.

## 2. Evidence

- [x] 2.1 Unit test: a restricted dome is twinned, and its `found_along` is its `escaped`.
- [x] 2.2 Integration tests: a smooth floor under a restricted dome renders the unrestricted dome within noise; light, bsdf and power agree on a twinned dome.
- [x] 2.3 The small Moana-water stage: restricted against unrestricted, 256 / 1 024 / 4 096 spp.
- [x] 2.4 Cost: render time and shadow rays with and without the twin.
- [x] 2.5 `scripts/check_images.sh`: every golden bit-identical.

## 3. Documentation

- [x] 3.1 The lighting design record, the site's limitations and `CRUST_LINK_TWIN` pages, the `docs/architecture.md` row.
