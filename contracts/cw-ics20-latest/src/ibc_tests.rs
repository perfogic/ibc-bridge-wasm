#[cfg(test)]
mod test {
    use cosmwasm_std::{coin, Addr, CosmosMsg, IbcTimeout, StdError};
    use cw20_ics20_msg::receiver::DestinationInfo;
    use oraiswap::asset::AssetInfo;
    use oraiswap::router::{RouterController, SwapOperation};

    use crate::ibc::{
        build_ibc_msg, build_swap_msgs, convert_remote_denom_to_evm_prefix, deduct_fee,
        deduct_relayer_fee, deduct_token_fee, get_swap_token_amount_out_from_orai,
        ibc_packet_receive, parse_ibc_channel_without_sanity_checks,
        parse_ibc_denom_without_sanity_checks, parse_voucher_denom, process_ibc_msg, Ics20Ack,
        Ics20Packet, FOLLOW_UP_IBC_SEND_FAILURE_ID, IBC_TRANSFER_NATIVE_ERROR_ID,
        NATIVE_RECEIVE_ID, SWAP_OPS_FAILURE_ID,
    };
    use crate::ibc::{build_swap_operations, get_follow_up_msgs};
    use crate::test_helpers::*;
    use cosmwasm_std::{
        from_binary, to_binary, IbcEndpoint, IbcMsg, IbcPacket, IbcPacketReceiveMsg, SubMsg,
        Timestamp, Uint128, WasmMsg,
    };

    use crate::error::ContractError;
    use crate::state::{
        get_key_ics20_ibc_denom, increase_channel_balance, ChannelState, MappingMetadata, Ratio,
        CHANNEL_REVERSE_STATE, RELAYER_FEE, TOKEN_FEE,
    };
    use cw20::{Cw20Coin, Cw20ExecuteMsg};
    use cw20_ics20_msg::amount::{convert_local_to_remote, Amount};

    use crate::contract::execute;
    use crate::msg::{ExecuteMsg, UpdatePairMsg};
    use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info};
    use cosmwasm_std::{coins, to_vec};

    #[test]
    fn check_ack_json() {
        let success = Ics20Ack::Result(b"1".into());
        let fail = Ics20Ack::Error("bad coin".into());

        let success_json = String::from_utf8(to_vec(&success).unwrap()).unwrap();
        assert_eq!(r#"{"result":"MQ=="}"#, success_json.as_str());

        let fail_json = String::from_utf8(to_vec(&fail).unwrap()).unwrap();
        assert_eq!(r#"{"error":"bad coin"}"#, fail_json.as_str());
    }

    #[test]
    fn test_sub_negative() {
        assert_eq!(
            Uint128::from(10u128)
                .checked_sub(Uint128::from(11u128))
                .unwrap_or_default(),
            Uint128::from(0u128)
        )
    }

    #[test]
    fn check_packet_json() {
        let packet = Ics20Packet::new(
            Uint128::new(12345),
            "ucosm",
            "cosmos1zedxv25ah8fksmg2lzrndrpkvsjqgk4zt5ff7n",
            "wasm1fucynrfkrt684pm8jrt8la5h2csvs5cnldcgqc",
            None,
        );
        // Example message generated from the SDK
        let expected = r#"{"amount":"12345","denom":"ucosm","receiver":"wasm1fucynrfkrt684pm8jrt8la5h2csvs5cnldcgqc","sender":"cosmos1zedxv25ah8fksmg2lzrndrpkvsjqgk4zt5ff7n","memo":null}"#;

        let encdoded = String::from_utf8(to_vec(&packet).unwrap()).unwrap();
        assert_eq!(expected, encdoded.as_str());
    }

    // #[test]
    // fn check_gas_limit_handles_all_cases() {
    //     let send_channel = "channel-9";
    //     let allowed = "foobar";
    //     let allowed_gas = 777666;
    //     let mut deps = setup(&[send_channel], &[(allowed, allowed_gas)]);

    //     // allow list will get proper gas
    //     let limit = check_gas_limit(deps.as_ref(), &Amount::cw20(500, allowed)).unwrap();
    //     assert_eq!(limit, Some(allowed_gas));

    //     // non-allow list will error
    //     let random = "tokenz";
    //     check_gas_limit(deps.as_ref(), &Amount::cw20(500, random)).unwrap_err();

    //     // add default_gas_limit
    //     let def_limit = 54321;
    //     migrate(
    //         deps.as_mut(),
    //         mock_env(),
    //         MigrateMsg {
    //             // default_gas_limit: Some(def_limit),
    //             // token_fee_receiver: "receiver".to_string(),
    //             // relayer_fee_receiver: "relayer_fee_receiver".to_string(),
    //             // default_timeout: 100u64,
    //             // fee_denom: "orai".to_string(),
    //             // swap_router_contract: "foobar".to_string(),
    //         },
    //     )
    //     .unwrap();

    //     // allow list still gets proper gas
    //     let limit = check_gas_limit(deps.as_ref(), &Amount::cw20(500, allowed)).unwrap();
    //     assert_eq!(limit, Some(allowed_gas));

    //     // non-allow list will now get default
    //     let limit = check_gas_limit(deps.as_ref(), &Amount::cw20(500, random)).unwrap();
    //     assert_eq!(limit, Some(def_limit));
    // }

    // test remote chain send native token to local chain
    fn mock_receive_packet_remote_to_local(
        my_channel: &str,
        amount: u128,
        denom: &str,
        receiver: &str,
        sender: Option<&str>,
    ) -> IbcPacket {
        let data = Ics20Packet {
            // this is returning a foreign native token, thus denom is <denom>, eg: uatom
            denom: denom.to_string(),
            amount: amount.into(),
            sender: if sender.is_none() {
                "remote-sender".to_string()
            } else {
                sender.unwrap().to_string()
            },
            receiver: receiver.to_string(),
            memo: None,
        };
        IbcPacket::new(
            to_binary(&data).unwrap(),
            IbcEndpoint {
                port_id: REMOTE_PORT.to_string(),
                channel_id: "channel-1234".to_string(),
            },
            IbcEndpoint {
                port_id: CONTRACT_PORT.to_string(),
                channel_id: my_channel.to_string(),
            },
            3,
            Timestamp::from_seconds(1665321069).into(),
        )
    }

    #[test]
    fn test_parse_voucher_denom_invalid_length() {
        let voucher_denom = "foobar/foobar";
        let ibc_endpoint = IbcEndpoint {
            port_id: "hello".to_string(),
            channel_id: "world".to_string(),
        };
        // native denom case
        assert_eq!(
            parse_voucher_denom(voucher_denom, &ibc_endpoint).unwrap_err(),
            ContractError::NoForeignTokens {}
        );
    }

    #[test]
    fn test_parse_voucher_denom_invalid_port() {
        let voucher_denom = "foobar/abc/xyz";
        let ibc_endpoint = IbcEndpoint {
            port_id: "hello".to_string(),
            channel_id: "world".to_string(),
        };
        // native denom case
        assert_eq!(
            parse_voucher_denom(voucher_denom, &ibc_endpoint).unwrap_err(),
            ContractError::FromOtherPort {
                port: "foobar".to_string()
            }
        );
    }

    #[test]
    fn test_parse_voucher_denom_invalid_channel() {
        let voucher_denom = "hello/abc/xyz";
        let ibc_endpoint = IbcEndpoint {
            port_id: "hello".to_string(),
            channel_id: "world".to_string(),
        };
        // native denom case
        assert_eq!(
            parse_voucher_denom(voucher_denom, &ibc_endpoint).unwrap_err(),
            ContractError::FromOtherChannel {
                channel: "abc".to_string()
            }
        );
    }

    #[test]
    fn test_parse_voucher_denom_native_denom_valid() {
        let voucher_denom = "foobar";
        let ibc_endpoint = IbcEndpoint {
            port_id: "hello".to_string(),
            channel_id: "world".to_string(),
        };
        assert_eq!(
            parse_voucher_denom(voucher_denom, &ibc_endpoint).unwrap(),
            ("foobar", true)
        );
    }

    /////////////////////////////// Test cases for native denom transfer from remote chain to local chain

    #[test]
    fn send_native_from_remote_mapping_not_found() {
        let relayer = Addr::unchecked("relayer");
        let send_channel = "channel-9";
        let cw20_addr = "token-addr";
        let custom_addr = "custom-addr";
        let cw20_denom = "cw20:token-addr";
        let gas_limit = 1234567;
        let mut deps = setup(
            &["channel-1", "channel-7", send_channel],
            &[(cw20_addr, gas_limit)],
        );

        // prepare some mock packets
        let recv_packet = mock_receive_packet_remote_to_local(
            send_channel,
            876543210,
            cw20_denom,
            custom_addr,
            None,
        );

        // we can receive this denom, channel balance should increase
        let msg = IbcPacketReceiveMsg::new(recv_packet.clone(), relayer);
        let res = ibc_packet_receive(deps.as_mut(), mock_env(), msg).unwrap();
        // assert_eq!(res, StdError)
        assert_eq!(
            res.attributes
                .into_iter()
                .find(|attr| attr.key.eq("error"))
                .unwrap()
                .value,
            "You can only send native tokens that has a map to the corresponding asset info"
        );
    }

    #[test]
    fn send_from_remote_to_local_receive_happy_path() {
        let relayer = Addr::unchecked("relayer");
        let send_channel = "channel-9";
        let cw20_addr = "token-addr";
        let custom_addr = "custom-addr";
        let denom = "uatom0x";
        let asset_info = AssetInfo::Token {
            contract_addr: Addr::unchecked(cw20_addr),
        };
        let gas_limit = 1234567;
        let send_amount = Uint128::from(876543210u64);
        let mut deps = setup(
            &["channel-1", "channel-7", send_channel],
            &[(cw20_addr, gas_limit)],
        );
        TOKEN_FEE
            .save(
                deps.as_mut().storage,
                denom,
                &Ratio {
                    nominator: 1,
                    denominator: 10,
                },
            )
            .unwrap();

        let pair = UpdatePairMsg {
            local_channel_id: send_channel.to_string(),
            denom: denom.to_string(),
            local_asset_info: asset_info.clone(),
            remote_decimals: 18u8,
            local_asset_info_decimals: 18u8,
        };

        let _ = execute(
            deps.as_mut(),
            mock_env(),
            mock_info("gov", &[]),
            ExecuteMsg::UpdateMappingPair(pair),
        )
        .unwrap();

        // prepare some mock packets
        let recv_packet = mock_receive_packet_remote_to_local(
            send_channel,
            send_amount.u128(),
            denom,
            custom_addr,
            Some("orai1cdhkt9ps47hwn9sqren70uw9cyrfka9fpauuks"),
        );

        // we can receive this denom, channel balance should increase
        let msg = IbcPacketReceiveMsg::new(recv_packet.clone(), relayer);
        let res = ibc_packet_receive(deps.as_mut(), mock_env(), msg).unwrap();
        println!("res: {:?}", res);
        // TODO: fix test cases. Possibly because we are adding two add_submessages?
        assert_eq!(res.messages.len(), 3); // 3 messages because we also have deduct fee msg and increase channel balance msg
        match res.messages[0].msg.clone() {
            CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr,
                msg,
                funds: _,
            }) => {
                assert_eq!(contract_addr, cw20_addr);
                assert_eq!(
                    msg,
                    to_binary(&Cw20ExecuteMsg::Transfer {
                        recipient: "gov".to_string(),
                        amount: Uint128::from(87654321u64) // send amount / token fee
                    })
                    .unwrap()
                );
            }
            _ => panic!("Unexpected return message: {:?}", res.messages[0]),
        }
        let ack: Ics20Ack = from_binary(&res.acknowledgement).unwrap();
        assert!(matches!(ack, Ics20Ack::Result(_)));

        // query channel state|_|
        match res.messages[1].msg.clone() {
            CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr,
                msg,
                funds: _,
            }) => {
                assert_eq!(contract_addr, "cosmos2contract".to_string()); // self-call msg
                assert_eq!(
                    msg,
                    to_binary(&ExecuteMsg::IncreaseChannelBalanceIbcReceive {
                        dest_channel_id: send_channel.to_string(),
                        ibc_denom: get_key_ics20_ibc_denom(CONTRACT_PORT, send_channel, denom),
                        amount: send_amount,
                        local_receiver: custom_addr.to_string(),
                    })
                    .unwrap()
                );
            }
            _ => panic!("Unexpected return message: {:?}", res.messages[0]),
        }
    }

    #[test]
    fn test_swap_operations() {
        let mut receiver_asset_info = AssetInfo::Token {
            contract_addr: Addr::unchecked("contract"),
        };
        let mut initial_asset_info = AssetInfo::Token {
            contract_addr: Addr::unchecked("addr"),
        };
        let fee_denom = "orai".to_string();

        let operations = build_swap_operations(
            receiver_asset_info.clone(),
            initial_asset_info.clone(),
            fee_denom.as_str(),
        );
        assert_eq!(operations.len(), 2);

        let fee_denom = "contract".to_string();
        receiver_asset_info = AssetInfo::NativeToken {
            denom: "contract".to_string(),
        };

        let operations = build_swap_operations(
            receiver_asset_info.clone(),
            initial_asset_info.clone(),
            &fee_denom,
        );
        assert_eq!(operations.len(), 1);
        assert_eq!(
            operations[0],
            SwapOperation::OraiSwap {
                offer_asset_info: initial_asset_info.clone(),
                ask_asset_info: AssetInfo::NativeToken {
                    denom: fee_denom.clone()
                }
            }
        );
        initial_asset_info = AssetInfo::NativeToken {
            denom: "contract".to_string(),
        };
        let operations = build_swap_operations(
            receiver_asset_info.clone(),
            initial_asset_info.clone(),
            &fee_denom,
        );
        assert_eq!(operations.len(), 0);

        initial_asset_info = AssetInfo::Token {
            contract_addr: Addr::unchecked("addr"),
        };
        let operations = build_swap_operations(
            receiver_asset_info.clone(),
            initial_asset_info.clone(),
            &fee_denom,
        );
        assert_eq!(operations.len(), 1);
        assert_eq!(
            operations[0],
            SwapOperation::OraiSwap {
                offer_asset_info: initial_asset_info.clone(),
                ask_asset_info: AssetInfo::NativeToken { denom: fee_denom }
            }
        );

        // initial = receiver => build swap ops length = 0
        let operations = build_swap_operations(
            AssetInfo::NativeToken {
                denom: "foobar".to_string(),
            },
            AssetInfo::NativeToken {
                denom: "foobar".to_string(),
            },
            "not_foo_bar",
        );
        assert_eq!(operations.len(), 0);
    }

    #[test]
    fn test_build_swap_msgs() {
        let minimum_receive = Uint128::from(10u128);
        let swap_router_contract = "router";
        let amount = Uint128::from(100u128);
        let mut initial_receive_asset_info = AssetInfo::Token {
            contract_addr: Addr::unchecked("addr"),
        };
        let native_denom = "foobar";
        let to: Option<Addr> = None;
        let mut cosmos_msgs: Vec<SubMsg> = vec![];
        let mut operations: Vec<SwapOperation> = vec![];
        build_swap_msgs(
            minimum_receive.clone(),
            &oraiswap::router::RouterController(swap_router_contract.to_string()),
            amount.clone(),
            initial_receive_asset_info.clone(),
            to.clone(),
            &mut cosmos_msgs,
            operations.clone(),
        )
        .unwrap();
        assert_eq!(cosmos_msgs.len(), 0);
        operations.push(SwapOperation::OraiSwap {
            offer_asset_info: initial_receive_asset_info.clone(),
            ask_asset_info: initial_receive_asset_info.clone(),
        });
        build_swap_msgs(
            minimum_receive.clone(),
            &oraiswap::router::RouterController(swap_router_contract.to_string()),
            amount.clone(),
            initial_receive_asset_info.clone(),
            to.clone(),
            &mut cosmos_msgs,
            operations.clone(),
        )
        .unwrap();
        // send in Cw20 send
        assert_eq!(true, format!("{:?}", cosmos_msgs[0]).contains("send"));

        // reset cosmos msg to continue testing
        cosmos_msgs.pop();
        initial_receive_asset_info = AssetInfo::NativeToken {
            denom: native_denom.to_string(),
        };
        build_swap_msgs(
            minimum_receive.clone(),
            &oraiswap::router::RouterController(swap_router_contract.to_string()),
            amount.clone(),
            initial_receive_asset_info.clone(),
            to.clone(),
            &mut cosmos_msgs,
            operations.clone(),
        )
        .unwrap();
        assert_eq!(
            true,
            format!("{:?}", cosmos_msgs[0]).contains("execute_swap_operations")
        );
        assert_eq!(
            SubMsg::reply_on_error(
                CosmosMsg::Wasm(WasmMsg::Execute {
                    contract_addr: swap_router_contract.to_string(),
                    msg: to_binary(&oraiswap::router::ExecuteMsg::ExecuteSwapOperations {
                        operations: operations,
                        minimum_receive: Some(minimum_receive),
                        to
                    })
                    .unwrap(),
                    funds: coins(amount.u128(), native_denom)
                }),
                SWAP_OPS_FAILURE_ID
            ),
            cosmos_msgs[0]
        );
    }

    #[test]
    fn test_build_swap_msgs_forbidden_case() {
        let minimum_receive = Uint128::from(10u128);
        let swap_router_contract = "router";
        let amount = Uint128::from(100u128);
        let initial_receive_asset_info = AssetInfo::Token {
            contract_addr: Addr::unchecked("addr"),
        };
        let mut cosmos_msgs: Vec<SubMsg> = vec![];
        let operations: Vec<SwapOperation> = vec![SwapOperation::OraiSwap {
            offer_asset_info: initial_receive_asset_info.clone(),
            ask_asset_info: initial_receive_asset_info.clone(),
        }];
        cosmos_msgs.push(SubMsg::new(CosmosMsg::Bank(cosmwasm_std::BankMsg::Send {
            to_address: "foobar".to_string(),
            amount: coins(1u128, "orai"),
        })));
        cosmos_msgs.push(SubMsg::new(CosmosMsg::Bank(cosmwasm_std::BankMsg::Send {
            to_address: "foobar".to_string(),
            amount: coins(1u128, "orai"),
        })));
        cosmos_msgs.push(SubMsg::new(CosmosMsg::Bank(cosmwasm_std::BankMsg::Send {
            to_address: "foobar".to_string(),
            amount: coins(1u128, "orai"),
        })));
        build_swap_msgs(
            minimum_receive.clone(),
            &oraiswap::router::RouterController(swap_router_contract.to_string()),
            amount.clone(),
            initial_receive_asset_info.clone(),
            Some(Addr::unchecked("attacker")),
            &mut cosmos_msgs,
            operations.clone(),
        )
        .unwrap();
        // should pop everything since 'to' is not None, and ops have items in it
        assert_eq!(cosmos_msgs.len(), 0);
    }

    #[test]
    fn test_get_ibc_msg_evm_case() {
        // setup
        let send_channel = "channel-9";
        let receive_channel = "channel-1";
        let allowed = "foobar";
        let pair_mapping_denom = "trx-mainnet0xa614f803B6FD780986A42c78Ec9c7f77e6DeD13C";
        let allowed_gas = 777666;
        let mut deps = setup(&[send_channel], &[(allowed, allowed_gas)]);
        let receiver_asset_info = AssetInfo::NativeToken {
            denom: "orai".to_string(),
        };
        let amount = Uint128::from(10u128);
        let remote_decimals = 18;
        let asset_info_decimals = 6;
        let remote_amount =
            convert_local_to_remote(amount, remote_decimals, asset_info_decimals).unwrap();
        let remote_address = "eth-mainnet0x1235";
        let mut env = mock_env();
        env.contract.address = Addr::unchecked("addr");
        let mut destination = DestinationInfo {
            receiver: "0x1234".to_string(),
            destination_channel: "channel-10".to_string(),
            destination_denom: "atom".to_string(),
        };
        let timeout = 1000u64;
        let local_receiver = "local_receiver";

        // first case, destination channel empty
        destination.destination_channel = "".to_string();

        let err = build_ibc_msg(
            env.clone(),
            local_receiver,
            receive_channel,
            amount,
            remote_address,
            &destination,
            timeout,
            None,
        )
        .unwrap_err();
        assert_eq!(
            err,
            StdError::generic_err("Destination channel empty in build ibc msg")
        );

        // evm based case, error getting pair mapping
        destination.receiver = "trx-mainnet0x73Ddc880916021EFC4754Cb42B53db6EAB1f9D64".to_string();
        destination.destination_channel = send_channel.to_string();
        let err = build_ibc_msg(
            env.clone(),
            local_receiver,
            receive_channel,
            amount,
            remote_address,
            &destination,
            timeout,
            None,
        )
        .unwrap_err();
        assert_eq!(err, StdError::generic_err("cannot find pair mappings"));

        // add a pair mapping so we can test the happy case evm based happy case
        let update = UpdatePairMsg {
            local_channel_id: "mars-channel".to_string(),
            denom: pair_mapping_denom.to_string(),
            local_asset_info: receiver_asset_info.clone(),
            remote_decimals,
            local_asset_info_decimals: asset_info_decimals,
        };

        // works with proper funds
        let msg = ExecuteMsg::UpdateMappingPair(update.clone());

        let info = mock_info("gov", &coins(1234567, "ucosm"));
        execute(deps.as_mut(), mock_env(), info, msg.clone()).unwrap();
        let pair_mapping_key = format!(
            "wasm.{}/{}/{}",
            "cosmos2contract", update.local_channel_id, pair_mapping_denom
        );
        increase_channel_balance(
            deps.as_mut().storage,
            receive_channel,
            pair_mapping_key.as_str(),
            remote_amount.clone(),
        )
        .unwrap();
        destination.receiver = "trx-mainnet0x73Ddc880916021EFC4754Cb42B53db6EAB1f9D64".to_string();
        destination.destination_channel = update.local_channel_id;
        let result = build_ibc_msg(
            env.clone(),
            local_receiver,
            receive_channel,
            amount,
            remote_address,
            &destination,
            timeout,
            Some((
                pair_mapping_key.clone(),
                MappingMetadata {
                    asset_info: receiver_asset_info.clone(),
                    remote_decimals,
                    asset_info_decimals: asset_info_decimals.clone(),
                },
            )),
        )
        .unwrap();

        assert_eq!(
            result[1],
            SubMsg::reply_on_error(
                CosmosMsg::Ibc(IbcMsg::SendPacket {
                    channel_id: receive_channel.to_string(),
                    data: to_binary(&Ics20Packet::new(
                        remote_amount.clone(),
                        pair_mapping_key.clone(),
                        env.contract.address.as_str(),
                        &remote_address,
                        Some(destination.receiver),
                    ))
                    .unwrap(),
                    timeout: env.block.time.plus_seconds(timeout).into()
                }),
                FOLLOW_UP_IBC_SEND_FAILURE_ID
            )
        );
        assert_eq!(
            result[0],
            SubMsg::new(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: env.contract.address.into_string(),
                msg: to_binary(&ExecuteMsg::ReduceChannelBalanceIbcReceive {
                    src_channel_id: receive_channel.to_string(),
                    ibc_denom: pair_mapping_key,
                    amount: remote_amount,
                    local_receiver: local_receiver.to_string()
                })
                .unwrap(),
                funds: vec![]
            }))
        );
    }

    #[test]
    fn test_get_ibc_msg_cosmos_based_case() {
        // setup
        let send_channel = "channel-10";
        let allowed = "foobar";
        let allowed_gas = 777666;
        let mut deps = setup(&[send_channel], &[(allowed, allowed_gas)]);
        let amount = Uint128::from(1000u64);
        let pair_mapping_denom = "cosmos1zedxv25ah8fksmg2lzrndrpkvsjqgk4zt5ff7n";
        let receiver_asset_info = AssetInfo::Token {
            contract_addr: Addr::unchecked("usdt"),
        };
        let local_channel_id = "channel";
        let local_receiver = "receiver";
        let timeout = 10u64;
        let remote_amount = convert_local_to_remote(amount.clone(), 18, 6).unwrap();
        let destination = DestinationInfo {
            receiver: "cosmos1zedxv25ah8fksmg2lzrndrpkvsjqgk4zt5ff7n".to_string(),
            destination_channel: send_channel.to_string(),
            destination_denom: "atom".to_string(),
        };
        let env = mock_env();
        let remote_address = "foobar";
        let ibc_denom = format!("foo/bar/{}", pair_mapping_denom);
        let remote_decimals = 18;
        let asset_info_decimals = 6;
        let pair_mapping_key = format!(
            "wasm.cosmos2contract/{}/{}",
            send_channel, pair_mapping_denom
        );

        CHANNEL_REVERSE_STATE
            .save(
                deps.as_mut().storage,
                (local_channel_id, ibc_denom.as_str()),
                &ChannelState {
                    outstanding: remote_amount.clone(),
                    total_sent: Uint128::from(100u128),
                },
            )
            .unwrap();

        CHANNEL_REVERSE_STATE
            .save(
                deps.as_mut().storage,
                (send_channel, pair_mapping_key.as_str()),
                &ChannelState {
                    outstanding: remote_amount.clone(),
                    total_sent: Uint128::from(100u128),
                },
            )
            .unwrap();

        // cosmos based case but no mapping found. should be successful & cosmos msg is ibc transfer
        let result = build_ibc_msg(
            env.clone(),
            local_receiver,
            local_channel_id,
            amount,
            remote_address,
            &destination,
            timeout,
            None,
        )
        .unwrap();
        assert_eq!(
            result[0],
            SubMsg::reply_on_error(
                CosmosMsg::Ibc(IbcMsg::Transfer {
                    channel_id: send_channel.to_string(),
                    to_address: destination.receiver.clone(),
                    amount: coin(1000u128, "atom"),
                    timeout: mock_env().block.time.plus_seconds(timeout).into()
                }),
                IBC_TRANSFER_NATIVE_ERROR_ID
            )
        );

        // cosmos based case with mapping found. Should be successful & cosmos msg is ibc send packet
        // add a pair mapping so we can test the happy case evm based happy case
        let update = UpdatePairMsg {
            local_channel_id: send_channel.to_string(),
            denom: pair_mapping_denom.to_string(),
            local_asset_info: receiver_asset_info.clone(),
            remote_decimals,
            local_asset_info_decimals: asset_info_decimals,
        };

        let msg = ExecuteMsg::UpdateMappingPair(update.clone());

        let info = mock_info("gov", &coins(1234567, "ucosm"));
        execute(deps.as_mut(), mock_env(), info, msg.clone()).unwrap();

        CHANNEL_REVERSE_STATE
            .save(
                deps.as_mut().storage,
                (local_channel_id, &pair_mapping_key),
                &ChannelState {
                    outstanding: remote_amount.clone(),
                    total_sent: Uint128::from(100u128),
                },
            )
            .unwrap();

        // now we get ibc msg
        let result = build_ibc_msg(
            env.clone(),
            local_receiver,
            local_channel_id,
            amount,
            remote_address,
            &destination,
            timeout,
            Some((
                pair_mapping_key.clone(),
                MappingMetadata {
                    asset_info: receiver_asset_info.clone(),
                    remote_decimals,
                    asset_info_decimals,
                },
            )),
        )
        .unwrap();

        assert_eq!(
            result[1],
            SubMsg::reply_on_error(
                CosmosMsg::Ibc(IbcMsg::SendPacket {
                    channel_id: send_channel.to_string(),
                    data: to_binary(&Ics20Packet::new(
                        remote_amount.clone(),
                        pair_mapping_key.clone(),
                        env.contract.address.as_str(),
                        &destination.receiver,
                        None,
                    ))
                    .unwrap(),
                    timeout: env.block.time.plus_seconds(timeout).into()
                }),
                FOLLOW_UP_IBC_SEND_FAILURE_ID
            )
        );
        assert_eq!(
            result[0],
            SubMsg::new(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: env.contract.address.into_string(),
                msg: to_binary(&ExecuteMsg::ReduceChannelBalanceIbcReceive {
                    src_channel_id: send_channel.to_string(),
                    ibc_denom: pair_mapping_key,
                    amount: remote_amount,
                    local_receiver: local_receiver.to_string()
                })
                .unwrap(),
                funds: vec![]
            }))
        );
    }

    #[test]
    fn test_get_ibc_msg_neither_cosmos_or_evm_based_case() {
        // setup
        let amount = Uint128::from(1000u64);
        let local_channel_id = "channel";
        let local_receiver = "receiver";
        let timeout = 10u64;
        let destination = DestinationInfo {
            receiver: "foo".to_string(),
            destination_channel: "channel-10".to_string(),
            destination_denom: "atom".to_string(),
        };
        let env = mock_env();
        let remote_address = "foobar";
        // cosmos based case but no mapping found. should be successful & cosmos msg is ibc transfer
        let result = build_ibc_msg(
            env.clone(),
            local_receiver,
            local_channel_id,
            amount,
            remote_address,
            &destination,
            timeout,
            None,
        )
        .unwrap_err();
        assert_eq!(
            result,
            StdError::generic_err("The destination info is neither evm or cosmos based")
        )
    }

    #[test]
    fn test_follow_up_msgs() {
        let send_channel = "channel-9";
        let local_channel = "channel";
        let allowed = "foobar";
        let allowed_gas = 777666;
        let mut deps = setup(&[send_channel], &[(allowed, allowed_gas)]);
        let deps_mut = deps.as_mut();
        let receiver = "foobar";
        let amount = Uint128::from(1u128);
        let mut env = mock_env();
        env.contract.address = Addr::unchecked("foobar");
        let initial_asset_info = AssetInfo::Token {
            contract_addr: Addr::unchecked("addr"),
        };

        // first case, memo empty => return send amount with receiver input
        let result = get_follow_up_msgs(
            deps_mut.storage,
            deps_mut.api,
            &deps_mut.querier,
            env.clone(),
            Amount::Cw20(Cw20Coin {
                address: "foobar".to_string(),
                amount: amount.clone(),
            }),
            initial_asset_info.clone(),
            AssetInfo::NativeToken {
                denom: "".to_string(),
            },
            "foobar",
            receiver.clone(),
            &DestinationInfo::from_str(""),
            local_channel,
            None,
        )
        .unwrap();

        assert_eq!(
            result.sub_msgs,
            vec![SubMsg::reply_on_error(
                CosmosMsg::Wasm(WasmMsg::Execute {
                    contract_addr: env.contract.address.to_string(),
                    msg: to_binary(&Cw20ExecuteMsg::Transfer {
                        recipient: receiver.to_string(),
                        amount: amount.clone()
                    })
                    .unwrap(),
                    funds: vec![]
                }),
                NATIVE_RECEIVE_ID
            )]
        );

        // 2nd case, destination denom is empty => destination is collected from memo
        let memo = "channel-15/cosmosabcd";
        let result = get_follow_up_msgs(
            deps_mut.storage,
            deps_mut.api,
            &deps_mut.querier,
            env.clone(),
            Amount::Cw20(Cw20Coin {
                address: "foobar".to_string(),
                amount,
            }),
            initial_asset_info.clone(),
            AssetInfo::NativeToken {
                denom: "cosmosabcd".to_string(),
            },
            "foobar",
            "foobar",
            &DestinationInfo::from_str(memo),
            local_channel,
            None,
        )
        .unwrap();

        assert_eq!(
            result.sub_msgs,
            vec![SubMsg::reply_on_error(
                CosmosMsg::Wasm(WasmMsg::Execute {
                    contract_addr: env.contract.address.to_string(),
                    msg: to_binary(&Cw20ExecuteMsg::Transfer {
                        recipient: receiver.to_string(),
                        amount: amount.clone()
                    })
                    .unwrap(),
                    funds: vec![]
                }),
                NATIVE_RECEIVE_ID
            )]
        );

        // 3rd case, cosmos msgs empty case, also send amount
        let memo = "cosmosabcd:orai";
        let result = get_follow_up_msgs(
            deps_mut.storage,
            deps_mut.api,
            &deps_mut.querier,
            env.clone(),
            Amount::Cw20(Cw20Coin {
                address: "foobar".to_string(),
                amount,
            }),
            AssetInfo::NativeToken {
                denom: "orai".to_string(),
            },
            AssetInfo::NativeToken {
                denom: "orai".to_string(),
            },
            "foobar",
            "foobar",
            &DestinationInfo::from_str(memo),
            local_channel,
            None,
        )
        .unwrap();

        assert_eq!(
            result.sub_msgs,
            vec![SubMsg::reply_on_error(
                CosmosMsg::Wasm(WasmMsg::Execute {
                    contract_addr: env.contract.address.to_string(),
                    msg: to_binary(&Cw20ExecuteMsg::Transfer {
                        recipient: receiver.to_string(),
                        amount: amount.clone()
                    })
                    .unwrap(),
                    funds: vec![]
                }),
                NATIVE_RECEIVE_ID
            )]
        );
    }

    #[test]
    fn test_deduct_fee() {
        assert_eq!(
            deduct_fee(
                Ratio {
                    nominator: 1,
                    denominator: 0,
                },
                Uint128::from(1000u64)
            ),
            Uint128::from(0u64)
        );
        assert_eq!(
            deduct_fee(
                Ratio {
                    nominator: 1,
                    denominator: 1,
                },
                Uint128::from(1000u64)
            ),
            Uint128::from(1000u64)
        );
        assert_eq!(
            deduct_fee(
                Ratio {
                    nominator: 1,
                    denominator: 100,
                },
                Uint128::from(1000u64)
            ),
            Uint128::from(10u64)
        );
    }

    #[test]
    fn test_convert_remote_denom_to_evm_prefix() {
        assert_eq!(convert_remote_denom_to_evm_prefix("abcd"), "".to_string());
        assert_eq!(convert_remote_denom_to_evm_prefix("0x"), "".to_string());
        assert_eq!(
            convert_remote_denom_to_evm_prefix("evm0x"),
            "evm".to_string()
        );
    }

    #[test]
    fn test_parse_ibc_denom_without_sanity_checks() {
        assert_eq!(parse_ibc_denom_without_sanity_checks("foo").is_err(), true);
        assert_eq!(
            parse_ibc_denom_without_sanity_checks("foo/bar").is_err(),
            true
        );
        let result = parse_ibc_denom_without_sanity_checks("foo/bar/helloworld").unwrap();
        assert_eq!(result, "helloworld");
    }

    #[test]
    fn test_parse_ibc_channel_without_sanity_checks() {
        assert_eq!(
            parse_ibc_channel_without_sanity_checks("foo").is_err(),
            true
        );
        assert_eq!(
            parse_ibc_channel_without_sanity_checks("foo/bar").is_err(),
            true
        );
        let result = parse_ibc_channel_without_sanity_checks("foo/bar/helloworld").unwrap();
        assert_eq!(result, "bar");
    }

    #[test]
    fn test_deduct_token_fee() {
        let mut deps = mock_dependencies();
        let amount = Uint128::from(1000u64);
        let storage = deps.as_mut().storage;
        let token_fee_denom = "foo0x";
        // should return amount because we have not set relayer fee yet
        assert_eq!(
            deduct_token_fee(storage, "foo", amount).unwrap().0,
            amount.clone()
        );
        TOKEN_FEE
            .save(
                storage,
                token_fee_denom,
                &Ratio {
                    nominator: 1,
                    denominator: 100,
                },
            )
            .unwrap();
        assert_eq!(
            deduct_token_fee(storage, token_fee_denom, amount)
                .unwrap()
                .0,
            Uint128::from(990u64)
        );
    }

    #[test]
    fn test_deduct_relayer_fee() {
        let mut deps = mock_dependencies();
        let deps_mut = deps.as_mut();
        let token_fee_denom = "cosmos";
        let remote_address = "cosmos1zedxv25ah8fksmg2lzrndrpkvsjqgk4zt5ff7n";
        let destination_asset_on_orai = AssetInfo::NativeToken {
            denom: "orai".to_string(),
        };
        let swap_router_contract = RouterController("foo".to_string());

        // token price empty case. Should return zero fee
        let result = deduct_relayer_fee(
            deps_mut.storage,
            deps_mut.api,
            &deps_mut.querier,
            remote_address,
            token_fee_denom,
            destination_asset_on_orai.clone(),
            &swap_router_contract,
        )
        .unwrap();
        assert_eq!(result, Uint128::from(0u64));

        // remote address is wrong (dont follow bech32 form)
        assert_eq!(
            deduct_relayer_fee(
                deps_mut.storage,
                deps_mut.api,
                &deps_mut.querier,
                "foobar",
                token_fee_denom,
                destination_asset_on_orai.clone(),
                &swap_router_contract,
            )
            .unwrap(),
            Uint128::from(0u128)
        );

        // no relayer fee case
        assert_eq!(
            deduct_relayer_fee(
                deps_mut.storage,
                deps_mut.api,
                &deps_mut.querier,
                remote_address,
                token_fee_denom,
                destination_asset_on_orai.clone(),
                &swap_router_contract,
            )
            .unwrap(),
            Uint128::from(0u64)
        );

        // oraib prefix case.
        RELAYER_FEE
            .save(deps_mut.storage, token_fee_denom, &Uint128::from(100u64))
            .unwrap();

        RELAYER_FEE
            .save(deps_mut.storage, "foo", &Uint128::from(1000u64))
            .unwrap();

        assert_eq!(
            deduct_relayer_fee(
                deps_mut.storage,
                deps_mut.api,
                &deps_mut.querier,
                "oraib1603j3e4juddh7cuhfquxspl0p0nsun047wz3rl",
                "foo0x",
                destination_asset_on_orai.clone(),
                &swap_router_contract,
            )
            .unwrap(),
            Uint128::from(1000u64)
        );

        // normal case with remote address
        assert_eq!(
            deduct_relayer_fee(
                deps_mut.storage,
                deps_mut.api,
                &deps_mut.querier,
                remote_address,
                token_fee_denom,
                destination_asset_on_orai,
                &swap_router_contract,
            )
            .unwrap(),
            Uint128::from(100u64)
        );
    }

    #[test]
    fn test_process_ibc_msg() {
        // setup
        let mut deps = mock_dependencies();
        let amount = Uint128::from(1000u64);
        let storage = deps.as_mut().storage;
        let ibc_denom = "foo/bar/cosmos";
        let pair_mapping = (
            ibc_denom.to_string(),
            MappingMetadata {
                asset_info: AssetInfo::NativeToken {
                    denom: "orai".to_string(),
                },
                remote_decimals: 18,
                asset_info_decimals: 6,
            },
        );
        let local_channel_id = "channel";
        let ibc_msg_sender = "sender";
        let ibc_msg_receiver = "receiver";
        let local_receiver = "local_receiver";
        let memo = None;
        let timeout = Timestamp::from_seconds(10u64);
        let remote_amount = convert_local_to_remote(amount.clone(), 18, 6).unwrap();

        CHANNEL_REVERSE_STATE
            .save(
                storage,
                (local_channel_id, ibc_denom),
                &ChannelState {
                    outstanding: remote_amount.clone(),
                    total_sent: Uint128::from(100u128),
                },
            )
            .unwrap();

        // action
        let result = process_ibc_msg(
            pair_mapping,
            mock_env().contract.address.into_string(),
            local_receiver,
            local_channel_id,
            ibc_msg_sender,
            ibc_msg_receiver,
            memo,
            amount,
            timeout,
        )
        .unwrap();

        assert_eq!(
            result[0],
            SubMsg::new(CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: mock_env().contract.address.into_string(),
                msg: to_binary(&ExecuteMsg::ReduceChannelBalanceIbcReceive {
                    src_channel_id: local_channel_id.to_string(),
                    ibc_denom: ibc_denom.to_string(),
                    amount: remote_amount,
                    local_receiver: local_receiver.to_string()
                })
                .unwrap(),
                funds: vec![]
            }))
        );

        assert_eq!(
            result[1],
            SubMsg::reply_on_error(
                IbcMsg::SendPacket {
                    channel_id: local_channel_id.to_string(),
                    data: to_binary(&Ics20Packet {
                        amount: remote_amount.clone(),
                        denom: ibc_denom.to_string(),
                        receiver: ibc_msg_receiver.to_string(),
                        sender: ibc_msg_sender.to_string(),
                        memo: None
                    })
                    .unwrap(),
                    timeout: IbcTimeout::with_timestamp(timeout)
                },
                FOLLOW_UP_IBC_SEND_FAILURE_ID
            )
        )
    }

    #[test]
    fn test_get_swap_token_amount_out_from_orai() {
        let deps = mock_dependencies();
        let simulate_amount = Uint128::from(10u128);
        let result = get_swap_token_amount_out_from_orai(
            &deps.as_ref().querier,
            simulate_amount,
            &RouterController("foo".to_string()),
            AssetInfo::NativeToken {
                denom: "orai".to_string(),
            },
        );
        assert_eq!(result, simulate_amount)
    }
}
