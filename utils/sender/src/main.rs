use clap::Parser;
use codec::Decode;
use log::*;
use sender_lib::{connect, sign_txs};
use std::error::Error;
use subxt::{ext::sp_core::Pair, utils::AccountId32, OnlineClient, PolkadotConfig};

const SENDER_SEED: &str = "//Sender";
const RECEIVER_SEED: &str = "//Receiver";

/// Util program to send transactions
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
	/// Node URL. Can be either a collator, or relaychain node based on whether you want to measure parachain TPS, or relaychain TPS.
	#[arg(long)]
	node_url: String,

	/// Set to the number of desired threads (default: 1). If set > 1 the program will spawn multiple threads to send transactions in parallel.
	#[arg(long, default_value_t = 1)]
	threads: usize,

	/// Total number of senders
	#[arg(long)]
	total_senders: Option<usize>,

	/// Chunk size for sending the extrinsics.
	#[arg(long, default_value_t = 50)]
	chunk_size: usize,

	/// Total number of pre-funded accounts (on funded-accounts.json).
	#[arg(long)]
	num: usize,
}

// FIXME: This assumes that all the chains supported by sTPS use this `AccountInfo` type. Currently,
// that holds. However, to benchmark a chain with another `AccountInfo` structure, a mechanism to
// adjust this type info should be provided.
type AccountInfo = frame_system::AccountInfo<u32, pallet_balances::AccountData<u128>>;

#[derive(Debug)]
enum AccountError {
	Subxt(subxt::Error),
	Codec,
}

impl std::fmt::Display for AccountError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			AccountError::Subxt(e) => write!(f, "Subxt error: {}", e.to_string()),
			AccountError::Codec => write!(f, "SCALE codec error"),
		}
	}
}

impl Error for AccountError {}

/// Check account nonce and free balance
async fn check_account(
	api: OnlineClient<PolkadotConfig>,
	account: AccountId32,
	ext_deposit: u128,
) -> Result<(), AccountError> {
	let account_state_storage_addr = subxt::dynamic::storage("System", "Account", vec![account]);
	let finalized_head_hash = api
		.backend()
		.latest_finalized_block_ref()
		.await
		.map_err(AccountError::Subxt)?
		.hash();
	let account_state_encoded = api
		.storage()
		.at(finalized_head_hash)
		.fetch(&account_state_storage_addr)
		.await
		.map_err(AccountError::Subxt)?
		.expect("Existantial deposit is set")
		.into_encoded();
	let account_state: AccountInfo =
		Decode::decode(&mut &account_state_encoded[..]).map_err(|_| AccountError::Codec)?;

	if account_state.nonce != 0 {
		panic!("Account has non-zero nonce");
	}

	// Reserve 10% for fees
	if (account_state.data.free as f64) < ext_deposit as f64 * 1.1 {
		panic!("Account has insufficient funds");
	}
	Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
	env_logger::init_from_env(
		env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
	);

	let args = Args::parse();

	if args.threads < 1 {
		panic!("Number of threads must be 1, or greater!")
	}

	// In case the optional total_senders argument is not passed for single-threaded mode,
	// we must make sure that we split the work evenly between threads for multi-threaded mode.
	let n_tx_sender = match args.total_senders {
		Some(tot_s) => args.num / tot_s,
		None => args.num / args.threads,
	};

	// Create the client here, so that we can use it in the various functions.
	let api = connect(&args.node_url).await?;

	let sender_accounts = funder_lib::derive_accounts(n_tx_sender, SENDER_SEED.to_owned());
	let receiver_accounts = funder_lib::derive_accounts(n_tx_sender, RECEIVER_SEED.to_owned());

	let ext_deposit_query = subxt::dynamic::constant("Balances", "ExistentialDeposit");
	let ext_deposit = api
		.constants()
		.at(&ext_deposit_query)?
		.to_value()?
		.as_u128()
		.expect("Only u128 deposits are supported");

	let mut precheck_set = tokio::task::JoinSet::new();
	sender_accounts.iter().for_each(|a| {
		let account_id: AccountId32 = a.public().into();
		let api = api.clone();
		precheck_set.spawn(check_account(api, account_id, ext_deposit));
	});
	while let Some(res) = precheck_set.join_next().await {
		res??;
	}

	let txs = sign_txs(api, sender_accounts.into_iter().zip(receiver_accounts.into_iter()))?;

	info!("Starting sender in parallel mode");
	// let (producer, consumer) = tokio::sync::mpsc::unbounded_channel();
	// I/O Bound
	// pre::parallel_pre_conditions(&api, args.threads, n_tx_sender).await?;
	// // CPU Bound
	// match sender_lib::parallel_signing(&api, args.threads, n_tx_sender, producer) {
	// 	Ok(_) => (),
	// 	Err(e) => panic!("Error: {:?}", e),
	// }
	// // I/O Bound
	sender_lib::submit_txs(txs, args.chunk_size).await?;

	Ok(())
}
