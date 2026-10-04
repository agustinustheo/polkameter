//! Ring-VRF proofs (bandersnatch, `verifiable` 0.3.0, the runtime's crate) for load txs.
//!
//! A flood proves every tx before it starts (immortal era), so proving limits setup time, not
//! the send rate. `open` (the ring setup for one member) is the slow part and does not depend on
//! the message, so [`Prover::open`] runs once per member and [`Opened::prove`] once per tx.

use rayon::prelude::*;
use verifiable::GenerateVerifiable;
use verifiable::ring::RingDomainSize;
use verifiable::ring::bandersnatch::BandersnatchVrfVerifiable as Vrf;

/// A single-context ring-VRF proof is 785 bytes.
pub const PROOF_BYTES: usize = 785;

/// A 32-byte member key (bandersnatch public key).
pub type Member = [u8; 32];

/// Proving failed: our member is not in the ring, or the ring is too big for its exponent.
#[derive(Debug, thiserror::Error)]
pub enum ProofError {
	/// No FFT domain for this ring exponent.
	#[error("unsupported ring exponent {0}")]
	Exponent(u8),
	/// `open` failed (member not in the ring, ring too large).
	#[error("ring open failed: {0:?}")]
	Open(verifiable::Error),
	/// `create` failed.
	#[error("proof create failed: {0:?}")]
	Create(verifiable::Error),
}

/// The ring sizes on chain: `RingExponent` 9 (People, 255 keys), 10 (recyclers, 767), 14.
pub fn domain(exponent: u8) -> Result<RingDomainSize, ProofError> {
	match exponent {
		9 => Ok(RingDomainSize::Domain11),
		10 => Ok(RingDomainSize::Domain12),
		14 => Ok(RingDomainSize::Domain16),
		other => Err(ProofError::Exponent(other)),
	}
}

/// The member key of `entropy`, as `verifiablejs.member_from_entropy`.
pub fn member_key(entropy: [u8; 32]) -> Member {
	Vrf::member_from_secret(&Vrf::new_secret(entropy))
}

/// A proof and the prover's alias in its context.
#[derive(Debug, Clone)]
pub struct RingProof {
	/// 785 bytes.
	pub proof: Vec<u8>,
	/// Alias in the proof's context.
	pub alias: [u8; 32],
}

/// A built ring: the keys its on-chain root covers, in ring order.
#[derive(Debug, Clone)]
pub struct Prover {
	domain: RingDomainSize,
	members: Vec<Member>,
}

/// One member with its ring opened: proves any number of messages.
pub struct Opened {
	entropy: [u8; 32],
	state: <Vrf as GenerateVerifiable>::Commitment,
}

impl std::fmt::Debug for Opened {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str("Opened")
	}
}

impl Prover {
	/// A prover over `members` of a ring with this exponent.
	pub fn new(exponent: u8, members: Vec<Member>) -> Result<Self, ProofError> {
		Ok(Self { domain: domain(exponent)?, members })
	}

	/// Opens the ring for the member of `entropy`.
	pub fn open(&self, entropy: [u8; 32]) -> Result<Opened, ProofError> {
		let state = Vrf::open(self.domain, &member_key(entropy), self.members.iter().copied())
			.map_err(ProofError::Open)?;
		Ok(Opened { entropy, state })
	}
}

impl Opened {
	/// Proves membership bound to `context` and `message`.
	pub fn prove(&self, context: &[u8], message: &[u8]) -> Result<RingProof, ProofError> {
		let secret = Vrf::new_secret(self.entropy);
		let (proof, alias) = Vrf::create(self.state.clone(), &secret, context, message)
			.map_err(ProofError::Create)?;
		let proof = proof.into_inner();
		debug_assert_eq!(proof.len(), PROOF_BYTES);
		Ok(RingProof { proof, alias })
	}
}

/// One proof to make.
#[derive(Debug, Clone)]
pub struct Job {
	/// Which opened member proves it (index into the `opened` slice).
	pub member: usize,
	/// Proof context.
	pub context: Vec<u8>,
	/// Proof message.
	pub message: [u8; 32],
}

/// Proving threads, one proof per thread (`verifiable` is built without `parallel`, so a proof
/// never starts threads of its own). Proof creation recurses deeply (ark FFTs), so the threads
/// get 64 MiB stacks; rayon's default (2 MiB) overflows.
#[derive(Debug)]
pub struct ProverPool {
	pool: rayon::ThreadPool,
}

const PROVER_STACK: usize = 64 << 20;

impl ProverPool {
	/// `threads` proving threads; `None` for all cores but one.
	pub fn new(threads: Option<usize>) -> Self {
		let threads = threads.unwrap_or_else(|| {
			std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1).max(1))
		});
		let pool = rayon::ThreadPoolBuilder::new()
			.num_threads(threads)
			.stack_size(PROVER_STACK)
			.thread_name(|i| format!("prover-{i}"))
			.build()
			.expect("prover threads start");
		Self { pool }
	}

	/// Proving threads.
	pub fn threads(&self) -> usize {
		self.pool.current_num_threads()
	}

	/// Makes every proof, in job order.
	pub fn prove_all(&self, opened: &[Opened], jobs: &[Job]) -> Result<Vec<RingProof>, ProofError> {
		self.pool.install(|| {
			jobs.par_iter()
				.map(|j| opened[j.member].prove(&j.context, &j.message))
				.collect()
		})
	}
}
