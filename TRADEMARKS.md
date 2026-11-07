# CodeSense name and logo

The CodeSense code is open source under the [MIT License](LICENSE). You can use,
change and share the code freely.

**The MIT License covers the code. It doesn't cover the CodeSense name, the
CodeSense logo and app icons, or the codesense.ai domain.** CodeSense™ and the
CodeSense logo are trademarks of Trevor Alfstad. This page explains when you can
use them.

Why: when someone downloads something called CodeSense, they should get the
signed app we ship, or know clearly that they're getting something else.

## You can, without asking

- Say your project **works with** CodeSense, or is **built for** or **based
  on** CodeSense, as long as you don't suggest we made it or endorse it.
- Write about, review, teach or compare CodeSense, in posts, talks, videos,
  courses and books.
- Name a dashboard, skill or agent team with a description such as "Release
  dashboard for CodeSense".
- Build CodeSense from source for yourself or inside your company, change it,
  and keep calling it CodeSense, as long as you don't give the build to anyone
  outside.
- Use the logo, unchanged, to link to codesense.ai.

## If you ship your own build

If you give a build of CodeSense to other people, changed or not, it must not
look like ours:

- **Give it a different name**, and don't use the CodeSense logo or app icons.
  You can say it's "based on CodeSense". Our app is `CodeSense.app`.
- **Change the bundle identifier** (ours is `ai.codesense`), so macOS treats it
  as a different app and it doesn't share CodeSense's settings, keychain items
  or data.
- **Point updates at your own server**, signed with your own key. Our update
  server only sends our builds, and your app can't install them anyway without
  our signing key.
- **Change the link scheme** (`codesense://`), so links meant for CodeSense
  don't open your app.

The README's
["Building your own release"](README.md#building-your-own-release) section
lists every setting to change and where it is.

## Ask us first

- Using CodeSense in the name of a product, company, service or domain (for
  example "CodeSense Cloud" or codesense-pro.com).
- Offering a hosted service under the CodeSense name.
- Putting the logo on merchandise or anything you sell.

## Never

- Suggest that you are us, or that we endorse you when we don't.
- Use the name or logo in a misleading way, or for malware, phishing or
  impersonation.
- Register "CodeSense", or a name confusingly like it, as a trademark or
  domain.

## Files the MIT License doesn't cover

These files are the CodeSense logo and app icon. They're in this repository so
the app can be built, but the MIT License doesn't let anyone use them as a
brand. A build you give to other people must replace them.

- `src-tauri/icons/`, except `src-tauri/icons/tray/`: the app icon in every
  size and format, including the master `source-icon.svg` and
  `source-icon*.png`, `icon.icns`, `icon.ico` and the generated Windows, iOS
  and Android sizes
- `public/favicon.png`: the app window's favicon

The menu-bar icons in `src-tauri/icons/tray/` and everything else in the
repository, including screenshots and avatar packs, are under the MIT License.

## Asking

Email [trademarks@codesense.ai](mailto:trademarks@codesense.ai). We say yes to
most things that help people use CodeSense.
