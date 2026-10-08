# Direct Agent sharing on iOS

The Share extension selects an Agent and sends only after explicit confirmation.
It does not launch the containing app using responder/private API workarounds.
System contact suggestions are not a prerequisite: the extension owns a searchable
recipient list and content preview, including filenames and sizes.

Only the currently validated connection's revocable session is exported to a
distinct Keychain service with explicit App Group access and
WhenUnlockedThisDeviceOnly protection. App Groups are a supported Keychain access
group; no raw bootstrap token, pending login proof or other profile is exported.
The host withdraws this capability on identity invalidation. The extension checks
an atomically written, credential-free authority generation as well as Keychain;
withdrawal precedes Keychain deletion/update so a locked/failed vault cannot
reactivate an old session after unlock. The extension checks
the item generation before and after network reads and immediately before POST,
then revalidates session user, protocol capabilities, runtime/visibility and
authoritative roster membership. LAN/IP HTTP retains the host's explicit
confirmation; the extension uses ATS's local-network exception rather than a
global arbitrary-load exception. HTTPS is supported for remote hosts; qualified
DNS HTTP is not covered by this exception.

Shared disk records contain content and destination identity, never credentials.
Before POST, the extension atomically records an immutable target and request UUID
and marks the outcome unknown. A lost reply or extension termination must retry
that same payload/UUID, not issue a new message. Confirmed receipts are persisted
before cleanup; retained accepted records cannot become new host imports. A
fallback host import must match the original runtime/user/visibility/network and
Agent, and uses the original UUID. Logout cannot undo a POST already admitted by
the server, so its record remains isolated rather than being reassigned.

This changes the previous staging-only boundary without adding background
polling, arbitrary URL dispatch, automatic sends or system contact-list access.
