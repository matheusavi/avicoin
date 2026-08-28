use crate::address::Address;
use crate::amount::Amount;
use crate::crypto::{PrivateKey, PubKeyHash, PublicKey, Signature};
use crate::params::Network;
use crate::script::p2pkh;
use crate::transaction::{Outpoint, Transaction, TxIn, TxOut, Witness};
use crate::util::hash160;
use crate::utxo::{Coin, UtxoSet};
use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::path::Path;

use crate::data_dir::DataDir;

// Everything from here to `TxBuilder` builds and signs a transaction, and
// nothing in the program calls it: the API deliberately has no `POST /send`,
// because spending authority behind a public URL is the one thing a node must
// not offer. #139 is the caller it is waiting for — a `send` subcommand that
// signs on the operator's own machine and posts what any stranger could have.

/// An output worth less than it costs to spend. Bitcoin's number, for an
/// output of the same shape: below this a change output is worth less than the
/// bytes that would later move it, so it is dropped into the fee instead.
#[allow(dead_code, reason = "the send path #139 gives a caller")]
pub const DUST: Amount = Amount::constant(546);

#[derive(Clone)]
pub struct Wallet {
    private_key: PrivateKey,
}

/// By its address, never its key: a `Node` derives `Debug`, and a private key
/// that can be printed is one that ends up in a log.
impl std::fmt::Debug for Wallet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Wallet({})", self.address())
    }
}

/// The key's own file, so a wallet is the same wallet tomorrow.
const KEY_FILE: &str = "wallet.key";

impl Wallet {
    pub fn new() -> Self {
        Wallet {
            private_key: PrivateKey::random(),
        }
    }

    /// The key in the data directory, minted on the first run and loaded on
    /// every one after. Without it a restart mines to a new address and
    /// yesterday's coins belong to nobody.
    ///
    /// **Plaintext**, at mode `0600`. Encryption would imply a security
    /// property nothing else here provides, and a passphrase prompt would
    /// imply a threat model this project does not have —
    /// [ADR-0013](../docs/adr/0013-persistence.md), and the README says so.
    pub fn stored(directory: &DataDir) -> Result<Wallet> {
        let path = directory.path().join(KEY_FILE);

        match fs::File::open(&path) {
            Ok(file) => {
                // The handle, not the path. Checking the mode with a second
                // `metadata(path)` would be checking a different file from the
                // one just read, which is the whole of the attack.
                only_ours(&file, &path)?;
                Ok(Wallet::of(parse_key(&read(file, &path)?).with_context(
                    || format!("{} does not hold a private key", path.display()),
                )?))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let wallet = Wallet::new();
                write_key(directory, &path, &wallet.private_key)?;
                Ok(wallet)
            }
            Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
        }
    }

    pub fn of(private_key: PrivateKey) -> Self {
        Wallet { private_key }
    }

    #[cfg(test)]
    pub fn key(&self) -> &PrivateKey {
        &self.private_key
    }

    pub fn public_key(&self) -> PublicKey {
        self.private_key.public_key()
    }

    pub fn pubkey_hash(&self) -> PubKeyHash {
        PubKeyHash::from_bytes(hash160(self.public_key().as_bytes()))
    }

    pub fn address(&self) -> Address {
        Address::for_pubkey_hash(self.pubkey_hash())
    }

    pub fn sign(&self, digest: &[u8; 32]) -> Signature {
        self.private_key.sign(digest)
    }

    /// Consensus accepts any script the interpreter validates; a wallet
    /// recognises one template. A non-standard output is perfectly valid and
    /// simply invisible here — ADR-0002.
    pub fn owns(&self, script_pubkey: &[u8]) -> bool {
        script_pubkey == p2pkh(&self.pubkey_hash())
    }

    /// Sorted largest first, so selection is deterministic rather than at the
    /// mercy of a hash map's ordering.
    pub fn spendable(
        &self,
        utxo: &UtxoSet,
        spend_height: u32,
        network: Network,
    ) -> Vec<(Outpoint, Coin)> {
        let mut mine: Vec<(Outpoint, Coin)> = utxo
            .coins()
            .into_iter()
            .filter(|(_, coin)| {
                self.owns(&coin.output.script_pubkey)
                    && coin.spendable_at(spend_height, network.maturity)
            })
            .collect();

        mine.sort_by(|(left_point, left), (right_point, right)| {
            right
                .output
                .value
                .cmp(&left.output.value)
                .then_with(|| {
                    left_point
                        .txid
                        .to_string()
                        .cmp(&right_point.txid.to_string())
                })
                .then_with(|| left_point.v_out.cmp(&right_point.v_out))
        });

        mine
    }

    /// Fallible because it has to be: ADR-0006 bounds each output, not the
    /// sum of what one wallet holds, and total supply is emergent rather than
    /// enforced — so a balance can leave the range no individual coin left.
    #[allow(dead_code, reason = "the send path #139 gives these a caller")]
    pub fn balance(&self, utxo: &UtxoSet, spend_height: u32, network: Network) -> Result<Amount> {
        Amount::sum(
            self.spendable(utxo, spend_height, network)
                .into_iter()
                .map(|(_, coin)| coin.output.value),
        )
        .ok_or_else(|| anyhow!("this wallet holds more than MAX_MONEY between it"))
    }

    #[allow(dead_code, reason = "the send path #139 gives these a caller")]
    pub fn build<'a>(
        &'a self,
        utxo: &'a UtxoSet,
        spend_height: u32,
        network: Network,
    ) -> TxBuilder<'a> {
        TxBuilder {
            wallet: self,
            utxo,
            spend_height,
            network,
            payments: Vec::new(),
            fee: Amount::ZERO,
        }
    }
}

/// The wallet's way of making a transaction. `sign` is its only output, so
/// nothing downstream holds one that is missing its witnesses.
#[allow(dead_code, reason = "the send path #139 gives these a caller")]
pub struct TxBuilder<'a> {
    wallet: &'a Wallet,
    utxo: &'a UtxoSet,
    spend_height: u32,
    network: Network,
    payments: Vec<TxOut>,
    fee: Amount,
}

#[allow(dead_code, reason = "the send path #139 gives these a caller")]
impl TxBuilder<'_> {
    /// Takes the address as text and decodes it here, so a mistyped one fails
    /// before anything is selected — and so no address reaches a
    /// `script_pubkey` without its checksum having been checked.
    pub fn pay(mut self, to: &str, amount: Amount) -> Result<Self> {
        let address: Address = to.parse().with_context(|| format!("paying {to}"))?;

        self.payments.push(TxOut {
            value: amount,
            script_pubkey: p2pkh(&address.pubkey_hash()),
        });

        Ok(self)
    }

    pub fn fee(mut self, fee: Amount) -> Self {
        self.fee = fee;
        self
    }

    pub fn sign(self) -> Result<Transaction> {
        if self.payments.is_empty() {
            bail!("a transaction pays someone");
        }

        let paying = Amount::sum(self.payments.iter().map(|output| output.value))
            .ok_or_else(|| anyhow!("the payments sum past MAX_MONEY"))?;
        let needed = paying
            .checked_add(self.fee)
            .ok_or_else(|| anyhow!("the payments and fee sum past MAX_MONEY"))?;

        let (selected, gathered) = self.select(needed)?;

        let mut outputs = self.payments.clone();
        let change = gathered
            .checked_sub(needed)
            .expect("selection stops once it covers what is needed");
        if change >= DUST {
            outputs.push(TxOut {
                value: change,
                script_pubkey: p2pkh(&self.wallet.pubkey_hash()),
            });
        }

        Ok(self.witness(selected, outputs))
    }

    fn select(&self, needed: Amount) -> Result<(Vec<Outpoint>, Amount)> {
        let mut gathered = Amount::ZERO;
        let mut selected = Vec::new();

        for (outpoint, coin) in self
            .wallet
            .spendable(self.utxo, self.spend_height, self.network)
        {
            selected.push(outpoint);
            gathered = gathered
                .checked_add(coin.output.value)
                .ok_or_else(|| anyhow!("the selected coins sum past MAX_MONEY"))?;

            if gathered >= needed {
                return Ok((selected, gathered));
            }
        }

        let short_by = needed
            .checked_sub(gathered)
            .expect("the loop only ends here when it did not cover what is needed");

        Err(anyhow!(
            "{needed} is {short_by} more than the {gathered} this wallet can spend"
        ))
    }

    /// One digest for every input: the txid, which the witnesses are not part
    /// of, so adding them cannot move what they signed — ADR-0004.
    fn witness(&self, spending: Vec<Outpoint>, outputs: Vec<TxOut>) -> Transaction {
        let mut transaction = Transaction {
            version: 1,
            inputs: spending
                .into_iter()
                .map(|previous_output| TxIn {
                    previous_output,
                    coinbase_data: Vec::new(),
                    witness: Witness::empty(),
                })
                .collect(),
            outputs,
        };

        let txid = transaction.get_tx_id();
        let signature = self.wallet.sign(txid.as_bytes());
        for input in &mut transaction.inputs {
            input.witness = Witness::new(vec![
                signature.as_bytes().to_vec(),
                self.wallet.public_key().as_bytes().to_vec(),
            ]);
        }

        transaction
    }
}

fn read(mut file: fs::File, path: &Path) -> Result<String> {
    use std::io::Read;

    let mut text = String::new();
    file.read_to_string(&mut text)
        .with_context(|| format!("could not read {}", path.display()))?;

    Ok(text)
}

fn parse_key(text: &str) -> Result<PrivateKey> {
    let text = text.trim();
    let material: [u8; 32] = hex::decode(text)
        .context("not hexadecimal")?
        .try_into()
        .map_err(|bytes: Vec<u8>| anyhow!("{} bytes, not 32", bytes.len()))?;

    PrivateKey::parse(&material)
}

/// Through a temporary name and a rename, both flushed. A key half-written by
/// a crash would not parse, and a key file that does not parse is refused for
/// the rest of the node's life — refusing is right, since the alternative is
/// discarding coins, so the write is what has to be all-or-nothing.
fn write_key(directory: &DataDir, path: &Path, key: &PrivateKey) -> Result<()> {
    use std::io::Write;

    let staged = path.with_extension("new");
    let mut file =
        create_narrow(&staged).with_context(|| format!("could not create {}", staged.display()))?;

    let written = (|| {
        writeln!(file, "{}", hex::encode(key.material()))?;
        file.sync_all()
    })();
    written.with_context(|| format!("could not write {}", staged.display()))?;
    drop(file);

    fs::rename(&staged, path)
        .with_context(|| format!("could not put {} in place", path.display()))?;

    // The rename itself, so a crash cannot leave a directory entry that never
    // reached the disk.
    fs::File::open(directory.path())
        .and_then(|dir| dir.sync_all())
        .with_context(|| format!("could not flush {}", directory.path().display()))
}

#[cfg(unix)]
fn create_narrow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    // `create_new` with the mode in the same call: creating it readable and
    // narrowing it afterwards leaves a window where it is not.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// No modes here. The permissions this file wants are a Unix idea, and every
/// document that claims `0600` says "on Unix" for that reason.
#[cfg(not(unix))]
fn create_narrow(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// A key anyone on the machine can read is not one worth loading quietly. The
/// file is refused rather than narrowed: whoever widened it may have copied it
/// already, and a node that silently fixed the mode would hide that.
#[cfg(unix)]
fn only_ours(file: &fs::File, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = file.metadata()?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "{} is mode {mode:o} — a private key anyone else can reach is not one to use",
            path.display()
        );
    }

    Ok(())
}

#[cfg(not(unix))]
fn only_ours(_file: &fs::File, _path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_dir::DataDir;
    use std::fs;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let path =
                std::env::temp_dir().join(format!("avicoin-wallet-{name}-{}", std::process::id()));
            fs::remove_dir_all(&path).ok();
            Scratch(path)
        }

        fn directory(&self) -> DataDir {
            DataDir::open(&self.0, &crate::params::MAINNET).unwrap()
        }

        fn key_file(&self) -> PathBuf {
            self.0.join(KEY_FILE)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn a_wallet_writes_its_key_on_the_first_run_and_loads_it_on_every_one_after() {
        let scratch = Scratch::new("persisted");
        let first = {
            let directory = scratch.directory();
            Wallet::stored(&directory).unwrap()
        };

        let directory = scratch.directory();
        let second = Wallet::stored(&directory).unwrap();

        assert_eq!(first.address().to_string(), second.address().to_string());
        assert_eq!(first.pubkey_hash(), second.pubkey_hash());
    }

    /// A key half-written by a crash would not parse, and a key file that does
    /// not parse is refused for the rest of the node's life. The write is
    /// therefore all-or-nothing: the staging file is what a crash catches.
    #[test]
    fn a_key_is_put_in_place_by_a_rename_and_never_written_where_it_is_read() {
        let scratch = Scratch::new("atomic");
        Wallet::stored(&scratch.directory()).unwrap();

        assert!(scratch.key_file().exists());
        assert!(
            !scratch.0.join("wallet.new").exists(),
            "the staging file is not left behind"
        );
    }

    #[test]
    fn two_directories_hold_two_wallets() {
        let one = Scratch::new("one");
        let two = Scratch::new("two");

        let first = Wallet::stored(&one.directory()).unwrap();
        let second = Wallet::stored(&two.directory()).unwrap();

        assert_ne!(first.address().to_string(), second.address().to_string());
    }

    #[cfg(unix)]
    #[test]
    fn a_key_is_written_readable_by_nobody_else() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("mode");
        Wallet::stored(&scratch.directory()).unwrap();

        let mode = fs::metadata(scratch.key_file())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(mode, 0o600, "got {mode:o}");
    }

    /// Refused rather than narrowed: whoever widened it may have copied it
    /// already, and a node that quietly fixed the mode would hide that.
    #[cfg(unix)]
    #[test]
    fn a_key_anyone_can_read_is_refused_rather_than_used() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("wide-open");
        Wallet::stored(&scratch.directory()).unwrap();
        fs::set_permissions(scratch.key_file(), fs::Permissions::from_mode(0o644)).unwrap();

        let error = format!("{:#}", Wallet::stored(&scratch.directory()).unwrap_err());

        assert!(error.contains("644"), "{error}");
        assert!(error.contains(KEY_FILE), "{error}");
    }

    #[rstest]
    #[case::not_hex("not-hex", "this is not a private key")]
    #[case::too_short("too-short", "00112233")]
    #[case::zero("zero", &"0".repeat(64))]
    fn a_key_file_that_is_not_a_key_is_an_error_naming_the_path(
        #[case] name: &str,
        #[case] contents: &str,
    ) {
        let scratch = Scratch::new(name);
        let directory = scratch.directory();
        fs::write(scratch.key_file(), contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(scratch.key_file(), fs::Permissions::from_mode(0o600)).unwrap();
        }

        let error = format!("{:#}", Wallet::stored(&directory).unwrap_err());

        assert!(error.contains(KEY_FILE), "{error}");
    }

    use crate::params::{MAINNET, TESTNET};
    use crate::validation::{check_spend, fixtures::funded};
    use rstest::rstest;

    const AFTER_MATURITY: u32 = 500;

    fn a_wallet_holding(atoms: &[u64]) -> (Wallet, UtxoSet) {
        let wallet = Wallet::new();
        let mut utxo = UtxoSet::new();
        for value in atoms {
            funded(&mut utxo, &wallet.private_key, *value, 0);
        }

        (wallet, utxo)
    }

    fn stranger() -> String {
        Wallet::new().address().to_string()
    }

    #[test]
    fn a_balance_is_the_sum_of_the_outputs_this_wallet_can_unlock() {
        let (wallet, utxo) = a_wallet_holding(&[100, 250, 7]);

        assert_eq!(
            wallet.balance(&utxo, AFTER_MATURITY, &MAINNET).unwrap(),
            Amount::from_atoms(357).unwrap()
        );
    }

    #[test]
    fn another_wallets_coins_are_not_in_this_ones_balance() {
        let (wallet, mut utxo) = a_wallet_holding(&[100]);
        funded(&mut utxo, &PrivateKey::random(), 9_000, 0);

        assert_eq!(
            wallet.balance(&utxo, AFTER_MATURITY, &MAINNET).unwrap(),
            Amount::from_atoms(100).unwrap()
        );
    }

    #[test]
    fn an_output_locked_to_a_script_this_wallet_does_not_know_is_invisible() {
        let (wallet, utxo) = a_wallet_holding(&[100]);

        assert!(
            !wallet.owns(&[0x51]),
            "a bare OP_TRUE is valid and not ours"
        );
        assert!(wallet.owns(&p2pkh(&wallet.pubkey_hash())));
        assert_eq!(wallet.spendable(&utxo, AFTER_MATURITY, &MAINNET).len(), 1);
    }

    #[test]
    fn an_immature_coinbase_is_not_part_of_a_balance() {
        let (wallet, utxo) = a_wallet_holding(&[100]);

        assert_eq!(
            wallet
                .balance(&utxo, MAINNET.maturity - 1, &MAINNET)
                .unwrap(),
            Amount::ZERO
        );
        assert_eq!(
            wallet.balance(&utxo, MAINNET.maturity, &MAINNET).unwrap(),
            Amount::from_atoms(100).unwrap()
        );
    }

    #[test]
    fn a_payment_the_balance_cannot_cover_builds_nothing_and_says_so() {
        let (wallet, utxo) = a_wallet_holding(&[100]);

        let error = format!(
            "{:#}",
            wallet
                .build(&utxo, AFTER_MATURITY, &MAINNET)
                .pay(&stranger(), Amount::from_atoms(5_000).unwrap())
                .unwrap()
                .sign()
                .unwrap_err()
        );

        assert!(error.contains("more than"), "{error}");
    }

    #[test]
    fn a_built_payment_validates_against_the_nodes_own_rules() {
        let (wallet, utxo) = a_wallet_holding(&[10_000]);

        let payment = wallet
            .build(&utxo, AFTER_MATURITY, &MAINNET)
            .pay(&stranger(), Amount::from_atoms(4_000).unwrap())
            .unwrap()
            .fee(Amount::from_atoms(100).unwrap())
            .sign()
            .unwrap();

        let fee = check_spend(&payment, &utxo, AFTER_MATURITY, &MAINNET).unwrap();
        assert_eq!(fee, Amount::from_atoms(100).unwrap());
    }

    #[test]
    fn change_comes_back_to_an_address_this_wallet_controls_and_can_be_spent_again() {
        let (wallet, mut utxo) = a_wallet_holding(&[10_000]);
        let first = wallet
            .build(&utxo, AFTER_MATURITY, &MAINNET)
            .pay(&stranger(), Amount::from_atoms(4_000).unwrap())
            .unwrap()
            .fee(Amount::from_atoms(100).unwrap())
            .sign()
            .unwrap();

        utxo.connect(&first, AFTER_MATURITY).unwrap();

        assert_eq!(
            wallet.balance(&utxo, AFTER_MATURITY + 1, &MAINNET).unwrap(),
            Amount::from_atoms(5_900).unwrap()
        );

        let second = wallet
            .build(&utxo, AFTER_MATURITY + 1, &MAINNET)
            .pay(&stranger(), Amount::from_atoms(5_000).unwrap())
            .unwrap()
            .sign()
            .unwrap();

        assert!(check_spend(&second, &utxo, AFTER_MATURITY + 1, &MAINNET).is_ok());
    }

    /// Change below the threshold is swept into the fee rather than becoming
    /// an output worth less than the bytes that would later move it.
    #[rstest]
    #[case::a_hair_under(DUST.atoms() - 1, 1)]
    #[case::exactly_the_threshold(DUST.atoms(), 2)]
    #[case::comfortably_over(DUST.atoms() + 1, 2)]
    fn dust_change_goes_to_the_fee_and_anything_larger_comes_home(
        #[case] change: u64,
        #[case] outputs: usize,
    ) {
        let (wallet, utxo) = a_wallet_holding(&[10_000]);
        let requested_fee = Amount::from_atoms(100).unwrap();
        let payment = wallet
            .build(&utxo, AFTER_MATURITY, &MAINNET)
            .pay(
                &stranger(),
                Amount::from_atoms(10_000 - 100 - change).unwrap(),
            )
            .unwrap()
            .fee(requested_fee)
            .sign()
            .unwrap();

        assert_eq!(payment.outputs.len(), outputs);
        assert!(payment.outputs.iter().all(|out| out.value > Amount::ZERO));

        let paid = check_spend(&payment, &utxo, AFTER_MATURITY, &MAINNET).unwrap();
        if outputs == 1 {
            assert_eq!(paid, Amount::from_atoms(100 + change).unwrap());
        } else {
            assert_eq!(paid, requested_fee);
            assert!(wallet.owns(&payment.outputs[1].script_pubkey));
        }
    }

    #[test]
    fn a_payment_that_takes_several_coins_takes_the_largest_first() {
        let (wallet, utxo) = a_wallet_holding(&[100, 5_000, 900]);

        let payment = wallet
            .build(&utxo, AFTER_MATURITY, &MAINNET)
            .pay(&stranger(), Amount::from_atoms(5_500).unwrap())
            .unwrap()
            .sign()
            .unwrap();

        assert_eq!(
            payment.inputs.len(),
            2,
            "5000 + 900 covers it; 100 is spare"
        );
        assert!(check_spend(&payment, &utxo, AFTER_MATURITY, &MAINNET).is_ok());
    }

    #[test]
    fn an_address_with_a_bad_checksum_fails_before_anything_is_selected() {
        let (wallet, utxo) = a_wallet_holding(&[10_000]);
        let mistyped: String = {
            let good = stranger();
            good.char_indices()
                .map(|(index, character)| if index == 5 { 'Z' } else { character })
                .collect()
        };

        assert!(wallet
            .build(&utxo, AFTER_MATURITY, &MAINNET)
            .pay(&mistyped, Amount::from_atoms(1).unwrap())
            .is_err());
    }

    #[test]
    fn a_transaction_paying_nobody_is_not_built() {
        let (wallet, utxo) = a_wallet_holding(&[10_000]);

        assert!(wallet
            .build(&utxo, AFTER_MATURITY, &MAINNET)
            .sign()
            .is_err());
    }

    #[test]
    fn a_signature_does_not_carry_to_another_transaction_spending_the_same_coin() {
        let (wallet, utxo) = a_wallet_holding(&[10_000]);
        let honest = wallet
            .build(&utxo, AFTER_MATURITY, &MAINNET)
            .pay(&stranger(), Amount::from_atoms(4_000).unwrap())
            .unwrap()
            .sign()
            .unwrap();
        let mut redirected = wallet
            .build(&utxo, AFTER_MATURITY, &MAINNET)
            .pay(&stranger(), Amount::from_atoms(4_000).unwrap())
            .unwrap()
            .sign()
            .unwrap();
        redirected.inputs[0].witness = honest.inputs[0].witness.clone();

        assert!(check_spend(&redirected, &utxo, AFTER_MATURITY, &MAINNET).is_err());
    }

    #[test]
    fn the_wallets_public_identity_is_the_address_of_its_key() {
        let wallet = Wallet::new();

        assert_eq!(
            wallet.address(),
            Address::for_public_key(&wallet.public_key())
        );
        assert_eq!(wallet.address().to_string().len(), 34);
    }

    #[test]
    fn a_balance_that_would_leave_the_range_is_an_error_rather_than_a_panic() {
        let (wallet, utxo) = a_wallet_holding(&[crate::amount::MAX_MONEY - 1, 2]);

        assert!(
            wallet.balance(&utxo, AFTER_MATURITY, &MAINNET).is_err(),
            "supply is emergent, not enforced: a wallet can hold more than MAX_MONEY"
        );
    }

    #[test]
    fn two_wallets_do_not_share_a_key() {
        assert_ne!(Wallet::new().public_key(), Wallet::new().public_key());
    }

    #[test]
    fn a_wallet_built_from_a_shipped_test_key_holds_the_allocation_it_was_given() {
        let genesis = TESTNET.genesis().unwrap();
        let mut utxo = UtxoSet::new();
        utxo.connect(&genesis.transactions[0], 0).unwrap();

        for key in crate::params::test_keys().unwrap() {
            let wallet = Wallet::of(key);

            assert!(wallet.balance(&utxo, TESTNET.maturity, &TESTNET).unwrap() > Amount::ZERO);
        }
    }
}
