+++
title = "tests must listen on 127.0.0.1, never on all interfaces (Windows firewall prompts)"
created = "2026-11-25"
evidence = "src-tauri/src/mnemo2/remote_hub.rs (bind_ip, new_loopback, tests_never_bind_all_interfaces); the user had to close two firewall dialogs on 2026-11-25"
+++

On Windows, a test executable that listens on 0.0.0.0 / Ipv4Addr::UNSPECIFIED raises a Windows Defender Firewall dialog. Every rebuilt test binary counts as a new program, so each check run can prompt the user again. Every test listener (TcpListener, HubServer HubOptions.bind, AppHub) must bind Ipv4Addr::LOCALHOST. Production code that must accept other computers (AppHub after the first pairing) keeps UNSPECIFIED behind a field, and tests construct it with the loopback variant (AppHub::new_loopback). remote_hub.rs has a guard test, tests_never_bind_all_interfaces. Loopback traffic is never blocked, so a dismissed dialog does not fail the tests.
