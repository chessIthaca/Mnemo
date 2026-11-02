+++
title = "Review Mnemo2 UI without screenshots"
created = "2027-01-11"
+++

User correction during Mnemo2 UI review: do not use screenshots because they keep failing. Review and verify UI changes through source inspection, DOM/state checks, and automated tests instead. Also keep using the existing Mnemo2 dev server on 5180; do not launch extra Chrome/WebView/CDP instances.
