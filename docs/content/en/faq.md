+++
title = "FAQ"
description = "Common questions about Mago, the project, and what does and does not belong in it."
nav_order = 10
nav_section = "Reference"
+++
# FAQ

## Why the name "Mago"?

The project was originally named "fennec", after the fennec fox native to North Africa. A name conflict with another tool forced a rename.

We picked "Mago" to stay close to our roots at Carthage Software. Mago of Carthage was an ancient Carthaginian writer known as the "Father of Agriculture". As he cultivated the land, the tool aims to help developers cultivate their codebases.

The name has a useful double meaning. In Spanish and Italian, "mago" means "magician" or "wizard". The logo captures both: a fennec fox in a wizard's hat and robe, with the ancient Carthaginian symbol of Tanit on its garments.

## How do you pronounce Mago?

`/ˈmɑːɡoʊ/`, "mah-go". Two syllables: "ma" as in "mama", "go" as in "go".

## Does mago-sharp ship a language server?

No. mago-sharp has no Language Server Protocol implementation. Run it from the command line or in CI. [Configuration](/guide/configuration/) covers the JSON schema editors use to validate `mago.toml`, and the terminal links that open a reported file in your editor.

## Does mago-sharp offer editor extensions (VS Code, etc.)?

No. mago-sharp ships no editor-specific extensions.

## Will Mago support analyzer plugins?

Yes, but not before `1.0.0`. The plan is for plugins to be written in Rust, compiled to WASM, and loaded by Mago at runtime. That work happens after `1.0.0` ships.

## What other PHP tools does Mago plan to replace?

The longer-term vision is for Mago to be a complete QA and development utility for PHP. The formatter, linter, and analyzer are the focus for `1.0.0`. Beyond that, planned tools include:

- A PHP version manager.
- A PHP extension installer.
- A migration helper for upgrading PHP versions, frameworks, or libraries.

## Will Mago implement a Composer alternative?

No. Composer is a fantastic tool, and most of its work is I/O-bound. A Rust rewrite would not gain much speed, would fragment the ecosystem, and would make it very difficult to support Composer's PHP-based plugin architecture.

## Will Mago implement a PHP runtime?

No. The PHP runtime is enormous. Even very large efforts (Facebook's HHVM, VK's KPHP) struggled to reach full parity with the Zend Engine. A smaller project cannot do better, and the result would only fragment the community. Mago focuses on tooling, not on runtimes.
