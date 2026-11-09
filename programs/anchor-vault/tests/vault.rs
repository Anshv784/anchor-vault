use anchor_lang::{
    prelude::Pubkey,
    solana_program::{instruction::Instruction, system_program},
    AccountDeserialize, InstructionData, ToAccountMetas,
};
use anchor_vault::state::VaultState;
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

const SOL: u64 = 1_000_000_000;

struct Setup {
    svm: LiteSVM,
    user: Keypair,
    vault_state: Pubkey,
    vault: Pubkey,
}

fn setup() -> Setup {
    let mut svm = LiteSVM::new();
    let bytes = include_bytes!(concat!(
        env!("CARGO_TARGET_TMPDIR"),
        "/../deploy/anchor_vault.so"
    ));
    svm.add_program(anchor_vault::id(), bytes).unwrap();

    let user = Keypair::new();
    svm.airdrop(&user.pubkey(), 10 * SOL).unwrap();

    let (vault_state, vault) = pdas(&user.pubkey());
    Setup {
        svm,
        user,
        vault_state,
        vault,
    }
}

fn pdas(user: &Pubkey) -> (Pubkey, Pubkey) {
    let (vault_state, _) =
        Pubkey::find_program_address(&[b"state", user.as_ref()], &anchor_vault::id());
    let (vault, _) =
        Pubkey::find_program_address(&[b"vault", vault_state.as_ref()], &anchor_vault::id());
    (vault_state, vault)
}

fn send(svm: &mut LiteSVM, ix: Instruction, signer: &Keypair) -> Result<(), String> {
    let msg = Message::new_with_blockhash(&[ix], Some(&signer.pubkey()), &svm.latest_blockhash());
    let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &[signer]).unwrap();
    let res = svm
        .send_transaction(tx)
        .map(|_| ())
        .map_err(|e| format!("{:?}", e.err));
    // new blockhash so the same ix can be sent twice in one test
    svm.expire_blockhash();
    res
}

fn initialize_ix(user: &Pubkey) -> Instruction {
    let (vault_state, vault) = pdas(user);
    Instruction::new_with_bytes(
        anchor_vault::id(),
        &anchor_vault::instruction::Initialize {}.data(),
        anchor_vault::accounts::Initialize {
            user: *user,
            vault_state,
            vault,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    )
}

fn payment_accounts(user: &Pubkey) -> Vec<anchor_lang::prelude::AccountMeta> {
    let (vault_state, vault) = pdas(user);
    anchor_vault::accounts::Payment {
        user: *user,
        vault_state,
        vault,
        system_program: system_program::ID,
    }
    .to_account_metas(None)
}

fn deposit_ix(user: &Pubkey, amount: u64) -> Instruction {
    Instruction::new_with_bytes(
        anchor_vault::id(),
        &anchor_vault::instruction::Deposit { amount }.data(),
        payment_accounts(user),
    )
}

fn withdraw_ix(user: &Pubkey, amount: u64) -> Instruction {
    Instruction::new_with_bytes(
        anchor_vault::id(),
        &anchor_vault::instruction::Withdraw { amount }.data(),
        payment_accounts(user),
    )
}

fn close_ix(user: &Pubkey) -> Instruction {
    let (vault_state, vault) = pdas(user);
    Instruction::new_with_bytes(
        anchor_vault::id(),
        &anchor_vault::instruction::Close {}.data(),
        anchor_vault::accounts::Close {
            user: *user,
            vault_state,
            vault,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    )
}

fn balance(svm: &LiteSVM, key: &Pubkey) -> u64 {
    svm.get_account(key).map(|a| a.lamports).unwrap_or(0)
}

fn rent_min(svm: &LiteSVM) -> u64 {
    svm.minimum_balance_for_rent_exemption(0)
}

#[test]
fn initialize_creates_state_and_funds_vault() {
    let Setup {
        mut svm,
        user,
        vault_state,
        vault,
    } = setup();

    send(&mut svm, initialize_ix(&user.pubkey()), &user).unwrap();

    let acc = svm.get_account(&vault_state).unwrap();
    assert_eq!(acc.owner, anchor_vault::id());
    let state = VaultState::try_deserialize(&mut acc.data.as_slice()).unwrap();

    let (_, state_bump) =
        Pubkey::find_program_address(&[b"state", user.pubkey().as_ref()], &anchor_vault::id());
    let (_, vault_bump) =
        Pubkey::find_program_address(&[b"vault", vault_state.as_ref()], &anchor_vault::id());
    assert_eq!(state.state_bump, state_bump);
    assert_eq!(state.vault_bump, vault_bump);

    // vault should be a system account holding exactly rent
    let vault_acc = svm.get_account(&vault).unwrap();
    assert_eq!(vault_acc.owner, system_program::ID);
    assert_eq!(vault_acc.lamports, rent_min(&svm));
}

#[test]
fn initialize_twice_fails() {
    let Setup { mut svm, user, .. } = setup();
    send(&mut svm, initialize_ix(&user.pubkey()), &user).unwrap();
    assert!(send(&mut svm, initialize_ix(&user.pubkey()), &user).is_err());
}

#[test]
fn deposit_moves_sol_into_vault() {
    let Setup {
        mut svm,
        user,
        vault,
        ..
    } = setup();
    send(&mut svm, initialize_ix(&user.pubkey()), &user).unwrap();

    let before = balance(&svm, &vault);
    send(&mut svm, deposit_ix(&user.pubkey(), 2 * SOL), &user).unwrap();
    send(&mut svm, deposit_ix(&user.pubkey(), SOL / 2), &user).unwrap();

    assert_eq!(balance(&svm, &vault), before + 2 * SOL + SOL / 2);
}

#[test]
fn deposit_zero_fails() {
    let Setup { mut svm, user, .. } = setup();
    send(&mut svm, initialize_ix(&user.pubkey()), &user).unwrap();

    let err = send(&mut svm, deposit_ix(&user.pubkey(), 0), &user).unwrap_err();
    assert!(err.contains("Custom(6000)"), "{err}");
}

#[test]
fn withdraw_sends_sol_back() {
    let Setup {
        mut svm,
        user,
        vault,
        ..
    } = setup();
    send(&mut svm, initialize_ix(&user.pubkey()), &user).unwrap();
    send(&mut svm, deposit_ix(&user.pubkey(), 3 * SOL), &user).unwrap();

    let user_before = balance(&svm, &user.pubkey());
    let vault_before = balance(&svm, &vault);

    send(&mut svm, withdraw_ix(&user.pubkey(), SOL), &user).unwrap();

    assert_eq!(balance(&svm, &vault), vault_before - SOL);
    // user pays a 5000 lamport fee for the tx
    assert_eq!(balance(&svm, &user.pubkey()), user_before + SOL - 5000);
}

#[test]
fn withdraw_everything_but_rent() {
    let Setup {
        mut svm,
        user,
        vault,
        ..
    } = setup();
    send(&mut svm, initialize_ix(&user.pubkey()), &user).unwrap();
    send(&mut svm, deposit_ix(&user.pubkey(), SOL), &user).unwrap();

    send(&mut svm, withdraw_ix(&user.pubkey(), SOL), &user).unwrap();
    assert_eq!(balance(&svm, &vault), rent_min(&svm));
}

#[test]
fn withdraw_more_than_deposited_fails() {
    let Setup {
        mut svm,
        user,
        vault,
        ..
    } = setup();
    send(&mut svm, initialize_ix(&user.pubkey()), &user).unwrap();
    send(&mut svm, deposit_ix(&user.pubkey(), SOL), &user).unwrap();

    // 1 lamport over would eat into the rent reserve
    let err = send(&mut svm, withdraw_ix(&user.pubkey(), SOL + 1), &user).unwrap_err();
    assert!(err.contains("Custom(6001)"), "{err}");
    assert_eq!(balance(&svm, &vault), SOL + rent_min(&svm));
}

#[test]
fn someone_else_cant_withdraw() {
    let Setup {
        mut svm,
        user,
        vault,
        vault_state,
    } = setup();
    send(&mut svm, initialize_ix(&user.pubkey()), &user).unwrap();
    send(&mut svm, deposit_ix(&user.pubkey(), 2 * SOL), &user).unwrap();

    let thief = Keypair::new();
    svm.airdrop(&thief.pubkey(), SOL).unwrap();

    // thief signs but passes the victim's vault accounts. the state seeds are
    // derived from the signer so they won't match
    let ix = Instruction::new_with_bytes(
        anchor_vault::id(),
        &anchor_vault::instruction::Withdraw { amount: SOL }.data(),
        anchor_vault::accounts::Payment {
            user: thief.pubkey(),
            vault_state,
            vault,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    );
    assert!(send(&mut svm, ix, &thief).is_err());
    assert_eq!(balance(&svm, &vault), 2 * SOL + rent_min(&svm));
}

#[test]
fn close_returns_everything() {
    let Setup {
        mut svm,
        user,
        vault,
        vault_state,
    } = setup();
    let start = balance(&svm, &user.pubkey());

    send(&mut svm, initialize_ix(&user.pubkey()), &user).unwrap();
    send(&mut svm, deposit_ix(&user.pubkey(), 4 * SOL), &user).unwrap();
    send(&mut svm, close_ix(&user.pubkey()), &user).unwrap();

    assert_eq!(balance(&svm, &vault), 0);
    assert_eq!(balance(&svm, &vault_state), 0);
    // everything comes back, only the 3 tx fees are gone
    assert_eq!(balance(&svm, &user.pubkey()), start - 3 * 5000);
}

#[test]
fn can_reinitialize_after_close() {
    let Setup { mut svm, user, .. } = setup();
    send(&mut svm, initialize_ix(&user.pubkey()), &user).unwrap();
    send(&mut svm, close_ix(&user.pubkey()), &user).unwrap();
    send(&mut svm, initialize_ix(&user.pubkey()), &user).unwrap();
    send(&mut svm, deposit_ix(&user.pubkey(), SOL), &user).unwrap();
}
