//! People v5 setup transaction signing.
use crate::tx::{ChainInfo, Ext, GeneralTx};
use polkameter_chain::Keypair;
/// `call` signed by `signer` with `nonce`.
pub fn signed(chain: &ChainInfo, signer: &Keypair, nonce: u32, call: Vec<u8>) -> Vec<u8> {
	sign(GeneralTx::new(chain, call).nonce(nonce), signer)
}

/// Signs a general tx with `VerifySignature::Signed` (coin transfers use it with `AsCoinage`).
pub fn sign(tx: GeneralTx<'_>, signer: &Keypair) -> Vec<u8> {
	let signature = signer.sign(&tx.message_after(Ext::VerifyMultiSignature));
	// VerifySignature::Signed(MultiSignature::Sr25519(signature), account)
	let verify = [&[1u8, 1][..], &signature.0, &signer.public_key().0].concat();
	tx.with(Ext::VerifyMultiSignature, verify).encode()
}
