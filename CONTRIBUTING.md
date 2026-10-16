# Contributing

Open an issue for a bug or describe a focused improvement before undertaking a large change. This project currently targets Windows desktop use, typically with 100–500 images. There is no public installer or cross-platform release commitment.

For a bug report, include the build/commit, Windows version, image format, source/destination filesystem, exact error and small reproduction steps. Replace personal paths and filenames with examples. If an image is needed, provide a disposable synthetic sample that you have permission to share. Do not upload saved profiles, journals, credentials or screenshots containing private images.

Use a branch and submit a pull request with the problem, behavior change and relevant validation. Run the checks in the README. A file-safety fix should include a regression showing the failure and correct behavior on disposable files. Preserve original-file identity checks, no-overwrite rules, durable acknowledgment and recovery/Undo restrictions; do not bypass them merely to make an operation succeed.

Keep changes small. Avoid broad simplification refactors, new dependencies or exhaustive performance programs without a concrete need. Existing tests and safety code should stay intact unless the change requires a reviewed replacement.

By submitting a contribution, you agree to license your project-authored contribution under the repository's MIT license. Preserve attribution and separate licenses when adding third-party material.
