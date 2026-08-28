use crate::amount::{Amount, ATOMS_PER_AVI};
use crate::script::p2pkh;
use crate::transaction::{Outpoint, TxOut, Txid};
use crate::utxo::{Coin, UtxoSet};
use crate::wallet::{Wallet, KEY_FILE};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

/// How long the node has to answer. It is a local process serving a handful of
/// JSON; a minute would be politeness to nobody.
const PATIENCE: Duration = Duration::from_secs(10);

/// A body the node could not have meant. `MAX_LISTED` bounds what `/address`
/// returns; this bounds what a wrong port could hand us.
const MAX_ANSWER: usize = 4 * 1024 * 1024;

/// Builds, signs and submits a payment — **on this machine**, with the key
/// never leaving it.
///
/// The API has no `POST /send` and should not: spending authority behind a
/// public URL is the one thing a node must not offer. So the split is that the
/// node holds the chain and this holds the key, and what crosses between them
/// is a signed transaction — the same thing any stranger could have sent.
pub fn send(data_dir: &Path, api: SocketAddr, to: &str, amount: Amount, fee: Amount) -> Result<()> {
    let wallet = Wallet::read(&data_dir.join(KEY_FILE)).with_context(|| {
        format!(
            "reading the key from {}; `send` never mints one, because a key the \
             node has not got is an address nobody will pay",
            data_dir.display()
        )
    })?;

    let status = ask(api, "/status")?;
    let network = crate::params::by_name(text(&status, "network")?)?;
    let height = number(&status, "height")? as u32;

    let address = wallet.address().to_string();
    let (utxo, held) = everything_held(api, &address, &wallet)?;

    // Before anything is selected or signed, and in the terms the operator
    // asked in: "short by 3.5 AVI" is a thing to act on, where "selection
    // failed" is not.
    let spendable = wallet.balance(&utxo, height + 1, network)?;
    let needed = amount
        .checked_add(fee)
        .context("the amount and the fee sum past MAX_MONEY")?;
    if spendable < needed {
        // What is *spendable*, which is not what the address holds: an
        // immature coinbase is held and cannot be spent, and saying the larger
        // number would send the operator looking for a coin that is there.
        let unripe = Amount::from_atoms(held.saturating_sub(spendable.atoms()))?;
        bail!(
            "{address} can spend {} AVI and this needs {} — short by {} AVI{}",
            spendable.in_avi(),
            needed.in_avi(),
            Amount::from_atoms(needed.atoms() - spendable.atoms())?.in_avi(),
            match unripe.atoms() {
                0 => String::new(),
                _ => format!(" ({} AVI is not mature yet)", unripe.in_avi()),
            }
        );
    }

    let payment = wallet
        .build(&utxo, height + 1, network)
        .pay(to, amount)?
        .fee(fee)
        .sign()?;

    let raw = payment.get_raw_format();
    let accepted = tell(api, "/tx", &hex::encode(&raw))?;

    println!("{}", text(&accepted, "txid")?);

    Ok(())
}

/// Every coin, not the first page of them. `/address` answers a page at a
/// time, and a wallet that can only see two hundred coins can only spend two
/// hundred — which a mining node passes in about two hundred blocks.
fn everything_held(api: SocketAddr, address: &str, wallet: &Wallet) -> Result<(UtxoSet, u64)> {
    let mut coins = Vec::new();
    let mut atoms;

    loop {
        let page = ask(api, &format!("/address/{address}?from={}", coins.len()))?;
        atoms = number(&page, "atoms")?;
        let held = number(&page, "unspent_count")? as usize;
        let listed = coins_of(&page, wallet)?;

        // An empty page ends it whatever the count says: a node whose set
        // moved between pages must not put this in a loop.
        if listed.is_empty() {
            break;
        }
        coins.extend(listed);

        if coins.len() >= held {
            break;
        }
    }

    Ok((UtxoSet::restored(coins), atoms))
}

/// What the node says this address holds, as coins the builder can select
/// from. The script is not in the answer and does not need to be: every coin
/// here pays this wallet, which is the only reason it is here.
fn coins_of(held: &Value, wallet: &Wallet) -> Result<Vec<(Outpoint, Coin)>> {
    let script = p2pkh(&wallet.pubkey_hash());
    let unspent = held["unspent"]
        .as_array()
        .context("the node's answer has no unspent list")?;

    let mut coins = Vec::with_capacity(unspent.len());
    for coin in unspent {
        let txid: [u8; 32] = hex::decode(text(coin, "txid")?)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .context("an unspent output names something that is not a txid")?;

        coins.push((
            Outpoint {
                // Big-endian on the wire out of the API, as every hash a
                // person reads is; reversed back at this edge and nowhere else.
                txid: Txid::from_bytes(crate::util::display_order(txid)),
                v_out: u32::try_from(number(coin, "index")?)
                    .ok()
                    .context("an unspent output's index is not one")?,
            },
            Coin {
                output: TxOut {
                    value: Amount::from_atoms(number(coin, "atoms")?)?,
                    script_pubkey: script.clone(),
                },
                height: number(coin, "height")? as u32,
                // Required, not defaulted. Defaulting to `false` would treat
                // an immature coinbase as spendable the day the API renamed
                // the field, and build a transaction the node then refuses.
                from_coinbase: coin["coinbase"]
                    .as_bool()
                    .context("an unspent output does not say whether it is a coinbase")?,
            },
        ));
    }

    Ok(coins)
}

/// AVI as a person writes it, into the atoms everything else counts in.
pub fn atoms_of(text: &str) -> Result<Amount> {
    let (whole, fraction) = match text.split_once('.') {
        // A trailing point is a typo, not a zero: `u64::from_str` takes "1."
        // apart happily and so would a right-pad.
        Some((_, "")) => bail!("{text} is not an amount"),
        Some(parts) => parts,
        None => (text, ""),
    };

    // Digits, and only digits. `u64::from_str` accepts a leading `+`, and the
    // right-pad below would turn a signed fraction into a different number
    // altogether — "1.+5" is 1.5 to nobody.
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        bail!("{text} is not an amount");
    }
    if fraction.len() > 8 {
        bail!("{text} is finer than an atom");
    }

    let whole: u64 = whole
        .parse()
        .with_context(|| format!("{text} is not an amount"))?;
    let padded = format!("{fraction:0<8}");
    let fraction: u64 = if fraction.is_empty() {
        0
    } else {
        padded
            .parse()
            .with_context(|| format!("{text} is not an amount"))?
    };

    whole
        .checked_mul(ATOMS_PER_AVI)
        .and_then(|atoms| atoms.checked_add(fraction))
        .ok_or_else(|| anyhow!("{text} is more than there will ever be"))
        .and_then(Amount::from_atoms)
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .with_context(|| format!("the node's answer has no {field}"))
}

fn number(value: &Value, field: &str) -> Result<u64> {
    value[field]
        .as_u64()
        .with_context(|| format!("the node's answer has no {field}"))
}

fn ask(api: SocketAddr, path: &str) -> Result<Value> {
    request(api, "GET", path, None)
}

/// The one thing another subcommand needs from here.
pub fn ask_status(api: SocketAddr) -> Result<Value> {
    ask(api, "/status")
}

fn tell(api: SocketAddr, path: &str, body: &str) -> Result<Value> {
    request(api, "POST", path, Some(body))
}

/// HTTP/1.1, the little of it this needs, for the same reason `api.rs` writes
/// its own: the alternative is a dependency for four requests.
fn request(api: SocketAddr, method: &str, path: &str, body: Option<&str>) -> Result<Value> {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {api}\r\nConnection: close\r\n");
    if let Some(body) = body {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");

    let mut stream = TcpStream::connect_timeout(&api, PATIENCE)
        .with_context(|| format!("no node answering on {api}"))?;
    stream.set_read_timeout(Some(PATIENCE))?;
    stream.set_write_timeout(Some(PATIENCE))?;

    stream.write_all(head.as_bytes())?;
    stream.write_all(body.unwrap_or("").as_bytes())?;

    let mut answer = Vec::new();
    stream
        .take(MAX_ANSWER as u64)
        .read_to_end(&mut answer)
        .with_context(|| format!("{api} said nothing within {PATIENCE:?}"))?;

    let text = String::from_utf8_lossy(&answer);
    let (head, payload) = text
        .split_once("\r\n\r\n")
        .context("the node did not answer with HTTP")?;
    let parsed: Value = serde_json::from_str(payload).context("the node's answer is not JSON")?;

    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .context("the node's answer has no status")?;

    if status != 200 {
        // The node's reason, not ours. It knows why it refused and we do not.
        bail!(
            "{api} refused: {}",
            parsed["error"].as_str().unwrap_or(payload)
        );
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("1", ATOMS_PER_AVI)]
    #[case("1.5", 150_000_000)]
    #[case("0.00000001", 1)]
    #[case("0", 0)]
    #[case("50.00000099", 5_000_000_099)]
    fn an_amount_in_avi_becomes_the_atoms_everything_counts_in(
        #[case] written: &str,
        #[case] atoms: u64,
    ) {
        assert_eq!(atoms_of(written).unwrap().atoms(), atoms);
    }

    #[rstest]
    #[case::finer_than_an_atom("0.000000001")]
    #[case::not_a_number("lots")]
    #[case::empty("")]
    #[case::negative("-1")]
    #[case::two_points("1.2.3")]
    #[case::past_max_money("999999999")]
    #[case::leading_plus("+1")]
    #[case::trailing_point("1.")]
    #[case::leading_point(".5")]
    #[case::signed_fraction("1.+5")]
    #[case::padded_signed_fraction("1.+0000001")]
    #[case::spaced(" 1")]
    #[case::scientific("1e5")]
    fn an_amount_that_is_not_one_is_refused(#[case] written: &str) {
        assert!(atoms_of(written).is_err(), "{written}");
    }

    /// A hash the API prints is big-endian; an outpoint is not. Reversing it
    /// back belongs at this edge and nowhere else — invariant 5.
    ///
    /// The txid is deliberately **not** a palindrome: `[7; 32]` reads the same
    /// either way, so a version of this that skipped the reversal entirely
    /// would pass.
    #[test]
    fn an_unspent_output_is_read_back_into_the_outpoint_it_names() {
        let wallet = Wallet::new();
        let mut bytes = [0u8; 32];
        for (at, byte) in bytes.iter_mut().enumerate() {
            *byte = at as u8;
        }
        let txid = Txid::from_bytes(bytes);
        let held = serde_json::json!({
            "unspent": [{
                "txid": txid.to_string(),
                "index": 3,
                "atoms": 5_000,
                "height": 9,
                "coinbase": true,
            }]
        });

        let coins = coins_of(&held, &wallet).unwrap();
        let (outpoint, coin) = &coins[0];

        assert_eq!(*outpoint, Outpoint { txid, v_out: 3 });

        assert_eq!(coin.output.value.atoms(), 5_000);
        assert_eq!(coin.output.script_pubkey, p2pkh(&wallet.pubkey_hash()));
        assert_eq!(coin.height, 9);
        assert!(coin.from_coinbase);
    }

    #[rstest]
    #[case::no_list(serde_json::json!({}))]
    #[case::not_a_txid(serde_json::json!({"unspent": [{"txid": "nope"}]}))]
    #[case::no_coinbase_flag(serde_json::json!({"unspent": [
        {"txid": "00".repeat(32), "index": 0, "atoms": 1, "height": 0}
    ]}))]
    #[case::index_past_a_u32(serde_json::json!({"unspent": [
        {"txid": "00".repeat(32), "index": 4_294_967_299u64, "atoms": 1, "height": 0,
         "coinbase": false}
    ]}))]
    fn an_answer_this_could_misread_is_refused_instead(#[case] held: serde_json::Value) {
        assert!(coins_of(&held, &Wallet::new()).is_err(), "{held}");
    }

    /// `send` must never mint a key. A key the node has not got is an address
    /// nobody will ever pay, and the coins would be lost the moment they were
    /// mined to it.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn a_missing_key_is_refused_rather_than_created() {
        let empty =
            Scratch(std::env::temp_dir().join(format!("avicoin-send-{}", std::process::id())));
        std::fs::create_dir_all(&empty.0).unwrap();
        let key = empty.0.join(KEY_FILE);
        std::fs::remove_file(&key).ok();

        let error = format!("{:#}", Wallet::read(&key).unwrap_err());

        assert!(error.contains("holds no wallet key"), "{error}");
        assert!(!key.exists(), "and nothing was written");
    }
}
