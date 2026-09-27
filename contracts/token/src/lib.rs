//! NearPaid launch token (lean build).
//!
//! A NEP-141 / NEP-145 fungible token written directly against the NEAR host
//! functions, without the SDK, so the shared global contract stays tiny (the
//! platform pays 10 NEAR per 100 KB once, burned). Features:
//!
//! * Standard `ft_*` and `storage_*` methods and NEP-297 events.
//! * `burn`: destroys the caller's own tokens; total supply goes down. The
//!   locker uses it to burn the coin side of pool fees.
//! * Optional holder rewards: the quote side of the coin's fees (native NEAR or
//!   one fungible token) is shared pro-rata across holders using
//!   reward-per-token accounting. The pool, locker and factory are excluded.
//! * Optional trade tax, fixed at launch (0-4% on buys, 0-4% on sells): charged
//!   only on transfers between the pool and a regular account. The burn share is
//!   destroyed at once; the creator and holder shares are parked in the factory's
//!   balance, which converts them into the pair's quote asset (`take_tax`).
//!
//! No owner, no mint after `new`, no pause, no upgrade path. `new` runs once,
//! in the same receipt that creates the account.

#![no_std]
extern crate alloc;

use alloc::vec::Vec;

// ================================================================ host API
#[allow(dead_code)]
mod sys {
    extern "C" {
        pub fn read_register(register_id: u64, ptr: u64);
        pub fn register_len(register_id: u64) -> u64;
        pub fn current_account_id(register_id: u64);
        pub fn predecessor_account_id(register_id: u64);
        pub fn input(register_id: u64);
        pub fn attached_deposit(balance_ptr: u64);
        pub fn prepaid_gas() -> u64;
        pub fn used_gas() -> u64;
        pub fn value_return(value_len: u64, value_ptr: u64);
        pub fn panic_utf8(len: u64, ptr: u64) -> !;
        pub fn log_utf8(len: u64, ptr: u64);
        pub fn promise_create(
            account_id_len: u64,
            account_id_ptr: u64,
            function_name_len: u64,
            function_name_ptr: u64,
            arguments_len: u64,
            arguments_ptr: u64,
            amount_ptr: u64,
            gas: u64,
        ) -> u64;
        pub fn promise_then(
            promise_index: u64,
            account_id_len: u64,
            account_id_ptr: u64,
            function_name_len: u64,
            function_name_ptr: u64,
            arguments_len: u64,
            arguments_ptr: u64,
            amount_ptr: u64,
            gas: u64,
        ) -> u64;
        pub fn promise_batch_create(account_id_len: u64, account_id_ptr: u64) -> u64;
        pub fn promise_batch_action_transfer(promise_index: u64, amount_ptr: u64);
        pub fn promise_results_count() -> u64;
        pub fn promise_result(result_idx: u64, register_id: u64) -> u64;
        pub fn promise_return(promise_id: u64);
        pub fn storage_write(key_len: u64, key_ptr: u64, value_len: u64, value_ptr: u64, register_id: u64) -> u64;
        pub fn storage_read(key_len: u64, key_ptr: u64, register_id: u64) -> u64;
        pub fn storage_remove(key_len: u64, key_ptr: u64, register_id: u64) -> u64;
        pub fn storage_has_key(key_len: u64, key_ptr: u64) -> u64;
    }
}

// ============================================================== allocator
#[cfg(target_arch = "wasm32")]
mod heap {
    use core::alloc::{GlobalAlloc, Layout};
    const PAGE: usize = 65_536;
    pub struct Bump;
    static mut NEXT: usize = 0;
    static mut END: usize = 0;
    unsafe impl GlobalAlloc for Bump {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            if END == 0 {
                let cur = core::arch::wasm32::memory_size(0) * PAGE;
                NEXT = cur;
                END = cur;
            }
            let start = (NEXT + l.align() - 1) & !(l.align() - 1);
            let next = start + l.size();
            if next > END {
                let pages = (next - END + PAGE - 1) / PAGE;
                if core::arch::wasm32::memory_grow(0, pages) == usize::MAX {
                    return core::ptr::null_mut();
                }
                END += pages * PAGE;
            }
            NEXT = next;
            start as *mut u8
        }
        unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
    }
    #[global_allocator]
    static A: Bump = Bump;

    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        super::fail(b"panic")
    }
}

// ============================================================== constants
const REG: u64 = 0;
const YOCTO_PER_BYTE: u128 = 10_000_000_000_000_000_000; // 1e19
const ACCOUNT_BYTES: u128 = 125;
const HOLDER_BYTES: u128 = 137;
const SCALE: u128 = 1_000_000_000_000_000_000_000_000; // 1e24
const TGAS: u64 = 1_000_000_000_000;
const GAS_RESOLVE: u64 = 10 * TGAS;
const GAS_RESERVE: u64 = 15 * TGAS; // resolve (10) + margin
const GAS_PAY: u64 = 10 * TGAS;
const GAS_PAID_CB: u64 = 10 * TGAS;

const K_SUPPLY: &[u8] = b"S";
const K_META: &[u8] = b"M";
const K_REWARD_ASSET: &[u8] = b"R"; // [0] native | [1] ++ ft account
const K_EXCLUDED: &[u8] = b"X"; // comma separated
const K_RPT: &[u8] = b"P";
const K_UNDIST: &[u8] = b"D";
const K_RECEIVED: &[u8] = b"T";
const K_CLAIMED: &[u8] = b"C";
const K_SOURCE: &[u8] = b"Q"; // account allowed to add rewards (the factory)
const K_TAX: &[u8] = b"F"; // [buy | sell | burn | creator | holders] as u16 LE bps
const K_POOL: &[u8] = b"L"; // the Rhea DCL account (tax applies to trades with it)
const K_TAX_C: &[u8] = b"G"; // creator share waiting in the factory's balance
const K_TAX_H: &[u8] = b"H"; // holder share waiting in the factory's balance
const K_TAX_B: &[u8] = b"B"; // tax burned to date
const K_ESCROW: &[u8] = b"E"; // sell tax held back until ft_resolve_transfer
const MAX_TAX_BPS: u128 = 400;

// ================================================================ helpers
fn fail(msg: &[u8]) -> ! {
    unsafe { sys::panic_utf8(msg.len() as u64, msg.as_ptr() as u64) }
}
fn require(cond: bool, msg: &[u8]) {
    if !cond {
        fail(msg)
    }
}
fn read_reg() -> Vec<u8> {
    let len = unsafe { sys::register_len(REG) };
    if len == u64::MAX {
        return Vec::new();
    }
    let mut v = alloc::vec![0u8; len as usize];
    unsafe { sys::read_register(REG, v.as_mut_ptr() as u64) };
    v
}
fn input() -> Vec<u8> {
    unsafe { sys::input(REG) };
    read_reg()
}
fn predecessor() -> Vec<u8> {
    unsafe { sys::predecessor_account_id(REG) };
    read_reg()
}
fn current() -> Vec<u8> {
    unsafe { sys::current_account_id(REG) };
    read_reg()
}
fn deposit() -> u128 {
    let mut b = [0u8; 16];
    unsafe { sys::attached_deposit(b.as_mut_ptr() as u64) };
    u128::from_le_bytes(b)
}
fn ret(v: &[u8]) {
    unsafe { sys::value_return(v.len() as u64, v.as_ptr() as u64) }
}
fn log(v: &[u8]) {
    unsafe { sys::log_utf8(v.len() as u64, v.as_ptr() as u64) }
}
fn sread(k: &[u8]) -> Option<Vec<u8>> {
    if unsafe { sys::storage_read(k.len() as u64, k.as_ptr() as u64, REG) } == 1 {
        Some(read_reg())
    } else {
        None
    }
}
fn swrite(k: &[u8], v: &[u8]) {
    unsafe { sys::storage_write(k.len() as u64, k.as_ptr() as u64, v.len() as u64, v.as_ptr() as u64, REG) };
}
fn sremove(k: &[u8]) {
    unsafe { sys::storage_remove(k.len() as u64, k.as_ptr() as u64, REG) };
}
fn shas(k: &[u8]) -> bool {
    unsafe { sys::storage_has_key(k.len() as u64, k.as_ptr() as u64) == 1 }
}
fn get_u128(k: &[u8]) -> u128 {
    sread(k).map(|v| le(&v, 0)).unwrap_or(0)
}
fn set_u128(k: &[u8], v: u128) {
    swrite(k, &v.to_le_bytes())
}
fn le(v: &[u8], at: usize) -> u128 {
    let mut b = [0u8; 16];
    b.copy_from_slice(&v[at..at + 16]);
    u128::from_le_bytes(b)
}
fn key(prefix: u8, acct: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(acct.len() + 1);
    k.push(prefix);
    k.extend_from_slice(acct);
    k
}
fn assert_one_yocto() {
    require(deposit() == 1, b"Requires attached deposit of exactly 1 yoctoNEAR");
}
fn assert_private() {
    require(predecessor() == current(), b"Method is private");
}
fn valid_account(a: &[u8]) -> bool {
    a.len() >= 2
        && a.len() <= 64
        && a.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-' || *c == b'_' || *c == b'.')
}
fn transfer_near(to: &[u8], amount: u128) -> u64 {
    unsafe {
        let p = sys::promise_batch_create(to.len() as u64, to.as_ptr() as u64);
        let a = amount.to_le_bytes();
        sys::promise_batch_action_transfer(p, a.as_ptr() as u64);
        p
    }
}
fn call(to: &[u8], method: &[u8], args: &[u8], deposit: u128, gas: u64) -> u64 {
    let d = deposit.to_le_bytes();
    unsafe {
        sys::promise_create(
            to.len() as u64, to.as_ptr() as u64, method.len() as u64, method.as_ptr() as u64,
            args.len() as u64, args.as_ptr() as u64, d.as_ptr() as u64, gas,
        )
    }
}
fn then(p: u64, to: &[u8], method: &[u8], args: &[u8], gas: u64) -> u64 {
    let d = 0u128.to_le_bytes();
    unsafe {
        sys::promise_then(
            p, to.len() as u64, to.as_ptr() as u64, method.len() as u64, method.as_ptr() as u64,
            args.len() as u64, args.as_ptr() as u64, d.as_ptr() as u64, gas,
        )
    }
}
fn promise_ok() -> Option<Vec<u8>> {
    unsafe {
        if sys::promise_results_count() == 1 && sys::promise_result(0, REG) == 1 {
            return Some(read_reg());
        }
    }
    None
}

// ---------------------------------------------------------------- numbers
fn u128_str(mut n: u128) -> Vec<u8> {
    if n == 0 {
        return alloc::vec![b'0'];
    }
    let mut buf = [0u8; 40];
    let mut i = buf.len();
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    buf[i..].to_vec()
}
fn parse_u128(s: &[u8]) -> Option<u128> {
    if s.is_empty() || s.len() > 39 {
        return None;
    }
    let mut n: u128 = 0;
    for c in s {
        if !c.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add((c - b'0') as u128)?;
    }
    Some(n)
}
/// floor(a * b / c) with a 256-bit intermediate.
fn mul_div(a: u128, b: u128, c: u128) -> u128 {
    mul_div_rem(a, b, c).0
}

/// ceil(a * b / c).
fn mul_div_ceil(a: u128, b: u128, c: u128) -> u128 {
    let (q, r) = mul_div_rem(a, b, c);
    if r { q + 1 } else { q }
}

/// (floor(a * b / c), remainder != 0) with a 256-bit intermediate.
fn mul_div_rem(a: u128, b: u128, c: u128) -> (u128, bool) {
    require(c > 0, b"division by zero");
    let (a1, a0) = (a >> 64, a & u64::MAX as u128);
    let (b1, b0) = (b >> 64, b & u64::MAX as u128);
    let p00 = a0 * b0;
    let p01 = a0 * b1;
    let p10 = a1 * b0;
    let p11 = a1 * b1;
    let mid = (p00 >> 64) + (p01 & u64::MAX as u128) + (p10 & u64::MAX as u128);
    let lo = (p00 & u64::MAX as u128) | (mid << 64);
    let hi = p11 + (p01 >> 64) + (p10 >> 64) + (mid >> 64);
    if hi == 0 {
        return (lo / c, lo % c != 0);
    }
    require(hi < c, b"mul_div overflow");
    let mut rem: u128 = hi;
    let mut q: u128 = 0;
    for i in (0..128).rev() {
        let carry = rem >> 127;
        rem = (rem << 1) | ((lo >> i) & 1);
        q <<= 1;
        if carry == 1 || rem >= c {
            rem = rem.wrapping_sub(c);
            q |= 1;
        }
    }
    (q, rem != 0)
}

// ------------------------------------------------------------------- json
enum Val {
    Str(Vec<u8>),
    Lit(Vec<u8>),
}
fn ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && matches!(s[i], b' ' | b'\n' | b'\r' | b'\t') {
        i += 1;
    }
    i
}
fn hex4(s: &[u8], i: usize) -> u32 {
    require(i + 4 <= s.len(), b"bad json escape");
    let mut v = 0u32;
    for c in &s[i..i + 4] {
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => fail(b"bad json escape"),
        };
        v = v * 16 + d as u32;
    }
    v
}
fn push_utf8(out: &mut Vec<u8>, cp: u32) {
    let mut b = [0u8; 4];
    let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
    out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
}
/// Parses a JSON string starting at the opening quote; returns (unescaped, index after).
fn jstr(s: &[u8], mut i: usize) -> (Vec<u8>, usize) {
    require(i < s.len() && s[i] == b'"', b"expected string");
    i += 1;
    let mut out = Vec::new();
    while i < s.len() {
        match s[i] {
            b'"' => return (out, i + 1),
            b'\\' => {
                require(i + 1 < s.len(), b"bad json");
                i += 1;
                match s[i] {
                    b'"' => out.push(b'"'),
                    b'\\' => out.push(b'\\'),
                    b'/' => out.push(b'/'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'u' => {
                        let mut cp = hex4(s, i + 1);
                        i += 4;
                        if (0xD800..0xDC00).contains(&cp) && i + 6 < s.len() && s[i + 1] == b'\\' && s[i + 2] == b'u' {
                            let lo = hex4(s, i + 3);
                            if (0xDC00..0xE000).contains(&lo) {
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                i += 6;
                            }
                        }
                        push_utf8(&mut out, cp);
                    }
                    _ => fail(b"bad json escape"),
                }
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    fail(b"unterminated string")
}
fn skip(s: &[u8], i: usize) -> usize {
    let i = ws(s, i);
    require(i < s.len(), b"bad json");
    match s[i] {
        b'"' => jstr(s, i).1,
        b'{' | b'[' => {
            let mut depth = 0i32;
            let mut j = i;
            while j < s.len() {
                match s[j] {
                    b'"' => {
                        j = jstr(s, j).1;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return j + 1;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            fail(b"bad json")
        }
        _ => {
            let mut j = i;
            while j < s.len() && !matches!(s[j], b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t') {
                j += 1;
            }
            j
        }
    }
}
/// Finds a top-level key of a JSON object.
fn arg(s: &[u8], name: &[u8]) -> Option<Val> {
    let mut i = ws(s, 0);
    if i >= s.len() || s[i] != b'{' {
        return None;
    }
    i += 1;
    loop {
        i = ws(s, i);
        if i >= s.len() || s[i] == b'}' {
            return None;
        }
        let (k, ni) = jstr(s, i);
        i = ws(s, ni);
        require(i < s.len() && s[i] == b':', b"bad json");
        i = ws(s, i + 1);
        let end = skip(s, i);
        if k == name {
            let v = if s[i] == b'"' { Val::Str(jstr(s, i).0) } else { Val::Lit(s[i..end].to_vec()) };
            require(!has_key_after(s, end, name), b"duplicate json key");
            return Some(v);
        }
        i = ws(s, end);
        if i < s.len() && s[i] == b',' {
            i += 1;
        }
    }
}
/// True if `name` appears again as a top-level key after position `i`.
fn has_key_after(s: &[u8], mut i: usize, name: &[u8]) -> bool {
    loop {
        i = ws(s, i);
        if i >= s.len() || s[i] == b'}' {
            return false;
        }
        if s[i] == b',' {
            i += 1;
            continue;
        }
        let (k, ni) = jstr(s, i);
        if k == name {
            return true;
        }
        i = ws(s, ni);
        require(i < s.len() && s[i] == b':', b"bad json");
        i = skip(s, i + 1);
    }
}

fn arg_str(s: &[u8], name: &[u8]) -> Option<Vec<u8>> {
    match arg(s, name) {
        Some(Val::Str(v)) => Some(v),
        _ => None,
    }
}
fn arg_acct(s: &[u8], name: &[u8]) -> Vec<u8> {
    let a = arg_str(s, name).unwrap_or_else(|| fail(b"missing account id"));
    require(valid_account(&a), b"invalid account id");
    a
}
fn arg_u128(s: &[u8], name: &[u8]) -> Option<u128> {
    match arg(s, name) {
        Some(Val::Str(v)) => Some(parse_u128(&v).unwrap_or_else(|| fail(b"invalid amount"))),
        _ => None,
    }
}
fn arg_bool(s: &[u8], name: &[u8]) -> Option<bool> {
    match arg(s, name) {
        Some(Val::Lit(v)) if v == b"true" => Some(true),
        Some(Val::Lit(v)) if v == b"false" => Some(false),
        _ => None,
    }
}
fn esc(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    for &c in s {
        match c {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            c if c < 0x20 => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.extend_from_slice(b"\\u00");
                out.push(HEX[(c >> 4) as usize]);
                out.push(HEX[(c & 15) as usize]);
            }
            c => out.push(c),
        }
    }
    out.push(b'"');
}
fn q128(n: u128) -> Vec<u8> {
    let mut v = Vec::new();
    v.push(b'"');
    v.extend_from_slice(&u128_str(n));
    v.push(b'"');
    v
}
/// Builds `{"k":"v",...}` from (key, raw json value) pairs.
fn obj(fields: &[(&[u8], Vec<u8>)]) -> Vec<u8> {
    let mut o = alloc::vec![b'{'];
    for (i, (k, v)) in fields.iter().enumerate() {
        if i > 0 {
            o.push(b',');
        }
        esc(&mut o, k);
        o.push(b':');
        o.extend_from_slice(v);
    }
    o.push(b'}');
    o
}
fn js(s: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    esc(&mut v, s);
    v
}
fn event(name: &[u8], data: Vec<u8>) {
    let mut e = b"EVENT_JSON:{\"standard\":\"nep141\",\"version\":\"1.0.0\",\"event\":\"".to_vec();
    e.extend_from_slice(name);
    e.extend_from_slice(b"\",\"data\":[");
    e.extend_from_slice(&data);
    e.extend_from_slice(b"]}");
    log(&e);
}

// ================================================================ ledger
fn balance(a: &[u8]) -> Option<u128> {
    sread(&key(b'b', a)).map(|v| le(&v, 0))
}
fn set_balance(a: &[u8], v: u128) {
    swrite(&key(b'b', a), &v.to_le_bytes())
}
fn rewards_on() -> bool {
    shas(K_REWARD_ASSET)
}
fn account_bytes() -> u128 {
    if rewards_on() { ACCOUNT_BYTES + HOLDER_BYTES } else { ACCOUNT_BYTES }
}
fn min_storage() -> u128 {
    account_bytes() * YOCTO_PER_BYTE
}
fn reward_asset() -> Option<Vec<u8>> {
    let v = sread(K_REWARD_ASSET)?;
    if v.first() == Some(&1) { Some(v[1..].to_vec()) } else { None }
}
fn excluded() -> Vec<Vec<u8>> {
    sread(K_EXCLUDED).map(|v| v.split(|c| *c == b',').filter(|p| !p.is_empty()).map(|p| p.to_vec()).collect()).unwrap_or_default()
}
fn is_excluded(a: &[u8]) -> bool {
    excluded().iter().any(|e| e.as_slice() == a)
}
fn eligible_supply() -> u128 {
    let ex: u128 = excluded().iter().map(|e| balance(e).unwrap_or(0)).sum();
    get_u128(K_SUPPLY).saturating_sub(ex).saturating_sub(get_u128(K_ESCROW))
}
/// Holder record: [paid rpt (16) | owed (16)].
fn holder(a: &[u8]) -> (u128, u128) {
    match sread(&key(b'h', a)) {
        Some(v) if v.len() == 32 => (le(&v, 0), le(&v, 16)),
        _ => (get_u128(K_RPT), 0),
    }
}
fn set_holder(a: &[u8], paid: u128, owed: u128) {
    let mut v = [0u8; 32];
    v[..16].copy_from_slice(&paid.to_le_bytes());
    v[16..].copy_from_slice(&owed.to_le_bytes());
    swrite(&key(b'h', a), &v);
}
fn pending(a: &[u8]) -> u128 {
    if !rewards_on() || is_excluded(a) {
        return 0;
    }
    let rpt = get_u128(K_RPT);
    let (paid, owed) = holder(a);
    owed + mul_div(balance(a).unwrap_or(0), rpt - paid, SCALE)
}
/// Moves rewards earned at the current balance into `owed`. Call before any balance change.
fn settle(a: &[u8]) {
    if !rewards_on() || is_excluded(a) || balance(a).is_none() {
        return;
    }
    let owed = pending(a);
    set_holder(a, get_u128(K_RPT), owed);
}
fn distribute(amount: u128) {
    set_u128(K_RECEIVED, get_u128(K_RECEIVED).saturating_add(amount));
    let pot = amount.saturating_add(get_u128(K_UNDIST));
    let eligible = eligible_supply();
    // Below 1M whole coins (1e24 raw) of eligible supply, hold the pot: a tiny
    // eligible supply would let one holder push the accumulator towards overflow.
    if eligible < SCALE {
        set_u128(K_UNDIST, pot);
        return;
    }
    let add = mul_div(pot, SCALE, eligible);
    let rpt = match get_u128(K_RPT).checked_add(add) {
        Some(v) => v,
        None => {
            set_u128(K_UNDIST, pot);
            return;
        }
    };
    // What the accumulator may hand out, rounded UP so the sum of all claims can
    // never exceed what was received; the rest is carried to the next distribution.
    let handed = mul_div_ceil(add, eligible, SCALE);
    set_u128(K_RPT, rpt);
    set_u128(K_UNDIST, pot - handed.min(pot));
    let mut l = b"rewards distributed: ".to_vec();
    l.extend_from_slice(&u128_str(pot));
    log(&l);
}
fn tax_cfg() -> Option<[u128; 5]> {
    let v = sread(K_TAX)?;
    let mut t = [0u128; 5];
    for (i, x) in t.iter_mut().enumerate() {
        *x = u16::from_le_bytes([v[2 * i], v[2 * i + 1]]) as u128;
    }
    Some(t)
}
/// Tax owed on `amount` moving sender -> receiver: the buy rate when the pool pays a
/// regular account, the sell rate when a regular account pays the pool, else 0.
/// Protocol accounts (pool, locker, factory) never pay it.
fn trade_tax(sender: &[u8], receiver: &[u8], amount: u128) -> u128 {
    let (t, pool) = match (tax_cfg(), sread(K_POOL)) {
        (Some(t), Some(p)) => (t, p),
        _ => return 0,
    };
    let bps = if sender == pool.as_slice() && !is_excluded(receiver) {
        t[0]
    } else if receiver == pool.as_slice() && !is_excluded(sender) {
        t[1]
    } else {
        0
    };
    mul_div(amount, bps, 10_000)
}
/// Settles a collected tax already taken out of `payer`'s balance: burns the burn
/// share, parks the creator + holder shares in the factory's balance.
fn apply_tax(payer: &[u8], tax: u128) {
    if tax == 0 {
        return;
    }
    let t = tax_cfg().unwrap_or([0; 5]);
    let burn = tax * t[2] / 10_000;
    let holders = tax * t[4] / 10_000;
    let creator = tax - burn - holders;
    if burn > 0 {
        set_u128(K_SUPPLY, get_u128(K_SUPPLY) - burn);
        set_u128(K_TAX_B, get_u128(K_TAX_B) + burn);
        event(b"ft_burn", obj(&[(b"owner_id", js(payer)), (b"amount", q128(burn)), (b"memo", js(b"tax"))]));
    }
    let kept = creator + holders;
    if kept > 0 {
        let sink = sread(K_SOURCE).unwrap_or_else(|| fail(b"no factory"));
        set_balance(&sink, balance(&sink).unwrap_or(0) + kept);
        set_u128(K_TAX_C, get_u128(K_TAX_C) + creator);
        set_u128(K_TAX_H, get_u128(K_TAX_H) + holders);
        event(b"ft_transfer", obj(&[
            (b"old_owner_id", js(payer)),
            (b"new_owner_id", js(&sink)),
            (b"amount", q128(kept)),
            (b"memo", js(b"tax")),
        ]));
    }
}
/// Moves `amount` out of the sender; the receiver gets it minus any trade tax.
/// With `escrow` the tax is only held back (settled in `ft_resolve_transfer`).
/// Returns (amount received, tax).
fn taxed_transfer(sender: &[u8], receiver: &[u8], amount: u128, memo: Option<Vec<u8>>, escrow: bool) -> (u128, u128) {
    let tax = trade_tax(sender, receiver, amount);
    let net = amount - tax;
    require(net > 0, b"The amount should be a positive number");
    internal_transfer(sender, receiver, amount, net, memo);
    if !escrow {
        apply_tax(sender, tax);
    }
    (net, tax)
}
fn internal_transfer(sender: &[u8], receiver: &[u8], amount: u128, credit: u128, memo: Option<Vec<u8>>) {
    require(amount > 0, b"The amount should be a positive number");
    require(sender != receiver, b"Sender and receiver should be different");
    require(valid_account(receiver), b"invalid account id");
    let sb = balance(sender).unwrap_or_else(|| fail(b"The account is not registered"));
    let rb = balance(receiver).unwrap_or_else(|| fail(b"The account is not registered"));
    require(sb >= amount, b"The account doesn't have enough balance");
    settle(sender);
    settle(receiver);
    set_balance(sender, sb - amount);
    set_balance(receiver, rb + credit);
    let mut f: Vec<(&[u8], Vec<u8>)> = alloc::vec![
        (b"old_owner_id", js(sender)),
        (b"new_owner_id", js(receiver)),
        (b"amount", q128(credit)),
    ];
    if let Some(m) = memo {
        f.push((b"memo", js(&m)));
    }
    event(b"ft_transfer", obj(&f));
}
fn storage_json() -> Vec<u8> {
    obj(&[(b"total", q128(min_storage())), (b"available", q128(0))])
}

// ============================================================== contract

/// Called once by the factory in the account-creating receipt.
/// Args: {"owner_id","total_supply","metadata": "<json>","rewards":"none"|"native"|"<ft>","excluded":"a,b,c","register":"a,b","reward_source":"<factory>",
///        "tax":"buy,sell,burn,creator,holders" (bps, optional),"pool":"<dcl>"}
#[no_mangle]
pub extern "C" fn new() {
    require(!shas(K_SUPPLY), b"Already initialized");
    let a = input();
    let owner = arg_acct(&a, b"owner_id");
    let supply = arg_u128(&a, b"total_supply").unwrap_or_else(|| fail(b"missing total_supply"));
    let meta = arg_str(&a, b"metadata").unwrap_or_else(|| fail(b"missing metadata"));
    swrite(K_META, &meta);
    set_u128(K_SUPPLY, supply);
    match arg_str(&a, b"rewards").as_deref() {
        None | Some(b"none") => {}
        Some(b"native") => swrite(K_REWARD_ASSET, &[0]),
        Some(ft) => {
            require(valid_account(ft), b"invalid reward asset");
            let mut v = alloc::vec![1u8];
            v.extend_from_slice(ft);
            swrite(K_REWARD_ASSET, &v);
        }
    }
    swrite(K_EXCLUDED, &arg_str(&a, b"excluded").unwrap_or_default());
    let src = arg_acct(&a, b"reward_source");
    swrite(K_SOURCE, &src);
    if let Some(cfg) = arg_str(&a, b"tax") {
        let mut t = [0u128; 5];
        let mut n = 0;
        for part in cfg.split(|c| *c == b',') {
            require(n < 5, b"tax: five numbers");
            t[n] = parse_u128(part).unwrap_or_else(|| fail(b"tax: bad number"));
            n += 1;
        }
        require(n == 5, b"tax: five numbers");
        require(t[0] <= MAX_TAX_BPS && t[1] <= MAX_TAX_BPS, b"tax: at most 4% a side");
        if t[0] + t[1] > 0 {
            require(t[2] + t[3] + t[4] == 10_000, b"tax: shares must add up to 100%");
            require(t[4] == 0 || rewards_on(), b"tax: holder share needs rewards");
            let pool = arg_acct(&a, b"pool");
            require(is_excluded(&pool) && is_excluded(&src), b"tax: pool and factory must be excluded");
            swrite(K_POOL, &pool);
            let mut v = [0u8; 10];
            for (i, x) in t.iter().enumerate() {
                v[2 * i..2 * i + 2].copy_from_slice(&(*x as u16).to_le_bytes());
            }
            swrite(K_TAX, &v);
        }
    }
    set_balance(&owner, supply);
    // Pre-register protocol accounts (pool, factory) so fees and swaps never bounce.
    for acct in arg_str(&a, b"register").unwrap_or_default().split(|c| *c == b',') {
        if valid_account(acct) && balance(acct).is_none() {
            set_balance(acct, 0);
            if rewards_on() && !is_excluded(acct) {
                set_holder(acct, 0, 0);
            }
        }
    }
    event(b"ft_mint", obj(&[(b"owner_id", js(&owner)), (b"amount", q128(supply)), (b"memo", js(b"launch"))]));
}

#[no_mangle]
pub extern "C" fn ft_transfer() {
    assert_one_yocto();
    let a = input();
    let receiver = arg_acct(&a, b"receiver_id");
    let amount = arg_u128(&a, b"amount").unwrap_or_else(|| fail(b"missing amount"));
    taxed_transfer(&predecessor(), &receiver, amount, arg_str(&a, b"memo"), false);
}

#[no_mangle]
pub extern "C" fn ft_transfer_call() {
    assert_one_yocto();
    let prepaid = unsafe { sys::prepaid_gas() };
    require(prepaid > GAS_RESERVE + 10 * TGAS, b"More gas is required");
    let a = input();
    let sender = predecessor();
    let receiver = arg_acct(&a, b"receiver_id");
    let amount = arg_u128(&a, b"amount").unwrap_or_else(|| fail(b"missing amount"));
    let msg = arg_str(&a, b"msg").unwrap_or_else(|| fail(b"missing msg"));
    // A sell's tax is held back until the pool reports how much it used.
    let (net, tax) = taxed_transfer(&sender, &receiver, amount, arg_str(&a, b"memo"), true);
    if tax > 0 {
        set_u128(K_ESCROW, get_u128(K_ESCROW) + tax);
    }
    let on_args = obj(&[(b"sender_id", js(&sender)), (b"amount", q128(net)), (b"msg", js(&msg))]);
    let used = unsafe { sys::used_gas() };
    let gas = prepaid.saturating_sub(used).saturating_sub(GAS_RESERVE);
    let p = call(&receiver, b"ft_on_transfer", &on_args, 0, gas);
    let res_args = obj(&[
        (b"sender_id", js(&sender)),
        (b"receiver_id", js(&receiver)),
        (b"amount", q128(net)),
        (b"tax", q128(tax)),
    ]);
    let p2 = then(p, &current(), b"ft_resolve_transfer", &res_args, GAS_RESOLVE);
    unsafe { sys::promise_return(p2) };
}

#[no_mangle]
pub extern "C" fn ft_resolve_transfer() {
    assert_private();
    let a = input();
    let sender = arg_str(&a, b"sender_id").unwrap_or_default();
    let receiver = arg_str(&a, b"receiver_id").unwrap_or_default();
    let amount = arg_u128(&a, b"amount").unwrap_or(0);
    let tax = arg_u128(&a, b"tax").unwrap_or(0);
    let unused = match promise_ok() {
        Some(v) => {
            let s = if v.len() >= 2 && v[0] == b'"' && v[v.len() - 1] == b'"' { v[1..v.len() - 1].to_vec() } else { v };
            parse_u128(&s).map(|u| u.min(amount)).unwrap_or(amount)
        }
        None => amount,
    };
    let mut refund = 0u128;
    if unused > 0 {
        let rb = balance(&receiver).unwrap_or(0);
        refund = unused.min(rb);
        if refund > 0 {
            settle(&receiver);
            set_balance(&receiver, rb - refund);
            match balance(&sender) {
                Some(sb) => {
                    settle(&sender);
                    set_balance(&sender, sb + refund);
                    event(b"ft_transfer", obj(&[
                        (b"old_owner_id", js(&receiver)),
                        (b"new_owner_id", js(&sender)),
                        (b"amount", q128(refund)),
                        (b"memo", js(b"refund")),
                    ]));
                }
                None => {
                    set_u128(K_SUPPLY, get_u128(K_SUPPLY) - refund);
                    event(b"ft_burn", obj(&[(b"owner_id", js(&receiver)), (b"amount", q128(refund))]));
                }
            }
        }
    }
    // Settle the held-back tax on the part the pool kept; return the rest.
    if tax > 0 {
        set_u128(K_ESCROW, get_u128(K_ESCROW).saturating_sub(tax));
        let kept = amount - unused;
        let owed = if amount == 0 { 0 } else { mul_div_ceil(tax, kept, amount).min(tax) };
        apply_tax(&sender, owed);
        let back = tax - owed;
        if back > 0 {
            match balance(&sender) {
                Some(sb) => {
                    settle(&sender);
                    set_balance(&sender, sb + back);
                }
                None => {
                    set_u128(K_SUPPLY, get_u128(K_SUPPLY) - back);
                    event(b"ft_burn", obj(&[(b"owner_id", js(&sender)), (b"amount", q128(back))]));
                }
            }
        }
        return ret(&q128(amount - refund + owed));
    }
    ret(&q128(amount - refund));
}

/// Factory only: hands over the creator and holder tax shares parked in its balance
/// (it converts them into the pair's quote asset) and resets the counters.
#[no_mangle]
pub extern "C" fn take_tax() {
    require(sread(K_SOURCE).as_deref() == Some(predecessor().as_slice()), b"Only the factory");
    let c = get_u128(K_TAX_C);
    let h = get_u128(K_TAX_H);
    set_u128(K_TAX_C, 0);
    set_u128(K_TAX_H, 0);
    ret(&obj(&[(b"creator", q128(c)), (b"holders", q128(h))]));
}

#[no_mangle]
pub extern "C" fn tax_info() {
    let t = match tax_cfg() {
        Some(t) => t,
        None => return ret(b"null"),
    };
    let n = |x: u128| u128_str(x);
    ret(&obj(&[
        (b"buy_bps", n(t[0])),
        (b"sell_bps", n(t[1])),
        (b"burn_bps", n(t[2])),
        (b"creator_bps", n(t[3])),
        (b"holders_bps", n(t[4])),
        (b"pending_creator", q128(get_u128(K_TAX_C))),
        (b"pending_holders", q128(get_u128(K_TAX_H))),
        (b"burned", q128(get_u128(K_TAX_B))),
    ]));
}

#[no_mangle]
pub extern "C" fn ft_total_supply() {
    ret(&q128(get_u128(K_SUPPLY)));
}

#[no_mangle]
pub extern "C" fn ft_balance_of() {
    let a = input();
    let acct = arg_str(&a, b"account_id").unwrap_or_default();
    ret(&q128(balance(&acct).unwrap_or(0)));
}

#[no_mangle]
pub extern "C" fn ft_metadata() {
    ret(&sread(K_META).unwrap_or_default());
}

/// Destroys the caller's own tokens. Total supply goes down.
#[no_mangle]
pub extern "C" fn burn() {
    assert_one_yocto();
    let a = input();
    let amount = arg_u128(&a, b"amount").unwrap_or(0);
    let owner = predecessor();
    let bal = balance(&owner).unwrap_or_else(|| fail(b"The account is not registered"));
    require(amount > 0 && bal >= amount, b"Nothing to burn or balance too low");
    settle(&owner);
    set_balance(&owner, bal - amount);
    set_u128(K_SUPPLY, get_u128(K_SUPPLY) - amount);
    event(b"ft_burn", obj(&[(b"owner_id", js(&owner)), (b"amount", q128(amount)), (b"memo", js(b"burn"))]));
}

// ---------------------------------------------------------------- NEP-145
#[no_mangle]
pub extern "C" fn storage_balance_bounds() {
    let m = q128(min_storage());
    ret(&obj(&[(b"min", m.clone()), (b"max", m)]));
}

#[no_mangle]
pub extern "C" fn storage_balance_of() {
    let a = input();
    let acct = arg_str(&a, b"account_id").unwrap_or_default();
    if balance(&acct).is_some() { ret(&storage_json()) } else { ret(b"null") }
}

#[no_mangle]
pub extern "C" fn storage_deposit() {
    let a = input();
    let caller = predecessor();
    let acct = match arg_str(&a, b"account_id") {
        Some(x) => {
            require(valid_account(&x), b"invalid account id");
            x
        }
        None => caller.clone(),
    };
    let amount = deposit();
    if balance(&acct).is_some() {
        if amount > 0 {
            transfer_near(&caller, amount);
        }
    } else {
        let min = min_storage();
        require(amount >= min, b"The attached deposit is less than the minimum storage balance");
        set_balance(&acct, 0);
        if rewards_on() && !is_excluded(&acct) {
            set_holder(&acct, get_u128(K_RPT), 0);
        }
        if amount > min {
            transfer_near(&caller, amount - min);
        }
    }
    ret(&storage_json());
}

#[no_mangle]
pub extern "C" fn storage_withdraw() {
    assert_one_yocto();
    let a = input();
    require(balance(&predecessor()).is_some(), b"The account is not registered");
    require(arg_u128(&a, b"amount").unwrap_or(0) == 0, b"The amount is greater than the available storage balance");
    ret(&storage_json());
}

#[no_mangle]
pub extern "C" fn storage_unregister() {
    assert_one_yocto();
    let a = input();
    let acct = predecessor();
    let force = arg_bool(&a, b"force").unwrap_or(false);
    let bal = match balance(&acct) {
        Some(b) => b,
        None => return ret(b"false"),
    };
    require(bal == 0 || force, b"Can't unregister the account with the positive balance without force");
    let unclaimed = pending(&acct);
    require(unclaimed == 0 || force, b"Claim holder rewards first or use force");
    if unclaimed > 0 {
        set_u128(K_UNDIST, get_u128(K_UNDIST).saturating_add(unclaimed));
    }
    if bal > 0 {
        settle(&acct);
        set_u128(K_SUPPLY, get_u128(K_SUPPLY) - bal);
        event(b"ft_burn", obj(&[(b"owner_id", js(&acct)), (b"amount", q128(bal))]));
    }
    let refund = min_storage();
    sremove(&key(b'b', &acct));
    sremove(&key(b'h', &acct));
    transfer_near(&acct, refund);
    ret(b"true");
}

// ---------------------------------------------------------- holder rewards

/// Native NEAR rewards (coins paired with NEAR). Only the factory may add.
#[no_mangle]
pub extern "C" fn deposit_rewards() {
    require(rewards_on() && reward_asset().is_none(), b"This coin does not take NEAR rewards");
    require(sread(K_SOURCE).as_deref() == Some(predecessor().as_slice()), b"Only the factory adds rewards");
    let amount = deposit();
    require(amount > 0, b"Attach NEAR");
    distribute(amount);
}

/// Reward-FT intake. Anything that is not the reward asset is returned.
#[no_mangle]
pub extern "C" fn ft_on_transfer() {
    let a = input();
    let amount = arg_u128(&a, b"amount").unwrap_or(0);
    let sender = arg_str(&a, b"sender_id").unwrap_or_default();
    let accepted = rewards_on()
        && reward_asset().as_deref() == Some(predecessor().as_slice())
        && sread(K_SOURCE).as_deref() == Some(sender.as_slice());
    if !accepted || amount == 0 {
        return ret(&q128(amount));
    }
    distribute(amount);
    ret(&q128(0));
}

fn claim_for(acct: Vec<u8>) {
    require(rewards_on(), b"No holder rewards");
    require(balance(&acct).is_some(), b"The account is not registered");
    settle(&acct);
    let (paid, owed) = holder(&acct);
    require(owed > 0, b"Nothing to claim");
    set_holder(&acct, paid, 0);
    set_u128(K_CLAIMED, get_u128(K_CLAIMED) + owed);
    let p = match reward_asset() {
        None => transfer_near(&acct, owed),
        Some(ft) => {
            let args = obj(&[(b"receiver_id", js(&acct)), (b"amount", q128(owed)), (b"memo", js(b"holder rewards"))]);
            call(&ft, b"ft_transfer", &args, 1, GAS_PAY)
        }
    };
    let cb = obj(&[(b"account_id", js(&acct)), (b"amount", q128(owed))]);
    let p2 = then(p, &current(), b"on_rewards_paid", &cb, GAS_PAID_CB);
    unsafe { sys::promise_return(p2) };
}

#[no_mangle]
pub extern "C" fn claim_rewards() {
    claim_for(predecessor());
}

/// Anyone may trigger a payout for an account; the money only goes to that account.
#[no_mangle]
pub extern "C" fn claim_rewards_for() {
    let a = input();
    claim_for(arg_acct(&a, b"account_id"));
}

#[no_mangle]
pub extern "C" fn on_rewards_paid() {
    assert_private();
    let a = input();
    let acct = arg_str(&a, b"account_id").unwrap_or_default();
    let amount = arg_u128(&a, b"amount").unwrap_or(0);
    if promise_ok().is_some() {
        return ret(b"true");
    }
    if balance(&acct).is_none() {
        set_u128(K_UNDIST, get_u128(K_UNDIST).saturating_add(amount));
        set_u128(K_CLAIMED, get_u128(K_CLAIMED) - amount);
        return ret(b"false");
    }
    let (paid, owed) = holder(&acct);
    set_holder(&acct, paid, owed + amount);
    set_u128(K_CLAIMED, get_u128(K_CLAIMED) - amount);
    ret(b"false");
}

#[no_mangle]
pub extern "C" fn pending_rewards() {
    let a = input();
    let acct = arg_str(&a, b"account_id").unwrap_or_default();
    ret(&q128(pending(&acct)));
}

#[no_mangle]
pub extern "C" fn rewards_info() {
    if !rewards_on() {
        return ret(b"null");
    }
    let asset = match reward_asset() {
        Some(ft) => js(&ft),
        None => b"null".to_vec(),
    };
    let mut ex = alloc::vec![b'['];
    for (i, e) in excluded().iter().enumerate() {
        if i > 0 {
            ex.push(b',');
        }
        esc(&mut ex, e);
    }
    ex.push(b']');
    ret(&obj(&[
        (b"asset", asset),
        (b"excluded", ex),
        (b"eligible_supply", q128(eligible_supply())),
        (b"total_received", q128(get_u128(K_RECEIVED))),
        (b"total_claimed", q128(get_u128(K_CLAIMED))),
        (b"undistributed", q128(get_u128(K_UNDIST))),
    ]));
}
