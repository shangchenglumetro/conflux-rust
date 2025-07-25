// Copyright 2019 Conflux Foundation. All rights reserved.
// Conflux is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use crate::{
    executive_observer::{AddressPocket, TracerTrait},
    stack::CallStackInfo,
    state::State,
    substate::Substate,
};
use cfx_vm_types::{self as vm, ActionParams, Spec};

use cfx_types::{
    address_util::AddressUtil, Address, AddressSpaceUtil, AddressWithSpace,
    Space,
};
use vm::Env;

use super::super::components::InternalRefContext;

fn available_admin_address(_spec: &Spec, address: &Address) -> bool {
    address.is_user_account_address() || address.is_null_address()
}

/// The Actual Implementation of `suicide`.
/// The contract which has non zero `collateral_for_storage` cannot suicide,
/// otherwise it will:
///   1. refund collateral for code
///   2. refund sponsor balance
///   3. refund contract balance
///   4. kill the contract
pub fn suicide(
    contract_address: &AddressWithSpace, refund_address: &AddressWithSpace,
    state: &mut State, spec: &Spec, substate: &mut Substate, env: &Env,
    creating_contract: bool, tracer: &mut dyn TracerTrait,
) -> vm::Result<()> {
    // After CIP-151, contract can only be killed in the same transaction as
    // its creation.
    let transaction_hash = env.transaction_hash;
    let contract_create_in_same_tx = state
        .created_at_transaction(contract_address, transaction_hash)?
        || creating_contract;
    let soft_suicide = spec.cip151 && !contract_create_in_same_tx;

    let balance = state.balance(contract_address)?;

    if !soft_suicide {
        substate.suicides.insert(contract_address.clone());
    }

    if (refund_address == contract_address && !soft_suicide)
        || (!spec.is_valid_address(&refund_address.address)
            && refund_address.space == Space::Native)
    {
        tracer.trace_internal_transfer(
            AddressPocket::Balance(*contract_address),
            AddressPocket::MintBurn,
            balance,
        );
        // When destroying, the balance will be burnt.
        state.sub_balance(contract_address, &balance)?;
        state.sub_total_issued(balance);
        if contract_address.space == Space::Ethereum {
            state.sub_total_evm_tokens(balance);
        }
    } else if refund_address != contract_address {
        trace!(target: "context", "Destroying {} -> {} (xfer: {})", contract_address.address, refund_address.address, balance);
        tracer.trace_internal_transfer(
            AddressPocket::Balance(*contract_address),
            AddressPocket::Balance(*refund_address),
            balance,
        );
        tracer.selfdestruct(
            contract_address.space,
            &contract_address.address,
            &refund_address.address,
            balance,
        );
        state.transfer_balance(contract_address, refund_address, &balance)?;
    }

    Ok(())
}

/// Implementation of `set_admin(address,address)`.
/// The input should consist of 20 bytes `contract_address` + 20 bytes
/// `new_admin_address`
pub fn set_admin(
    contract_address: Address, new_admin_address: Address,
    params: &ActionParams, context: &mut InternalRefContext,
) -> vm::Result<()> {
    let requester = &params.sender;
    debug!(
        "set_admin requester {:?} contract {:?}, \
         new_admin {:?}, admin_control_in_creation {:?}",
        requester,
        contract_address,
        new_admin_address,
        context.callstack.admin_control_in_creation(),
    );

    let clear_admin_in_create = context.callstack.admin_control_in_creation()
        == Some(&contract_address.with_native_space())
        && new_admin_address.is_null_address();

    if context.is_contract_address(&contract_address)?
        && context.state.exists(&contract_address.with_native_space())?
        // Allow set admin if requester matches or in contract creation to clear admin.
        && (context.state.admin(&contract_address)?.eq(requester)
        || clear_admin_in_create)
        // Only allow user account to be admin, if not to clear admin.
        && available_admin_address(&context.spec, &new_admin_address)
    {
        debug!("set_admin to {:?}", new_admin_address);
        // Admin is cleared by set new_admin_address to null address.
        context
            .state
            .set_admin(&contract_address, &new_admin_address)?;
    }
    Ok(())
}

/// Implementation of `destroy(address)`.
/// The input should consist of 20 bytes `contract_address`
pub fn destroy(
    contract_address: Address, params: &ActionParams, state: &mut State,
    spec: &Spec, substate: &mut Substate, env: &Env,
    tracer: &mut dyn TracerTrait, callstack: &CallStackInfo,
) -> vm::Result<()> {
    debug!("contract_address={:?}", contract_address);

    let requester = &params.sender;
    let admin = state.admin(&contract_address)?;
    if admin == *requester {
        suicide(
            &contract_address.with_native_space(),
            &admin.with_native_space(),
            state,
            spec,
            substate,
            env,
            callstack.creating_contract(&contract_address.with_native_space()),
            tracer,
        )
    } else {
        Ok(())
    }
}
