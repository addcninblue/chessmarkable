use crate::game::ChessGame;
use crate::game::Player as PlecoPlayer;
pub use crate::game::{ChessOutcome, SQ};
use crate::{Player, Square, BitMoveWrapper};
use anyhow::{Context, Result};
use chess_pgn_parser::Game;
use pleco::tools::Searcher;
pub use pleco::BitMove;
use serde::{Deserialize, Serialize};
use std::thread;
use std::time::{Duration, SystemTime};
use tokio::stream::StreamExt;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::task;

#[derive(Clone, Debug)]
pub struct ChessConfig {
    pub starting_fen: Option<String>,
    pub can_black_undo: bool,
    pub can_white_undo: bool,
    pub allow_undo_after_loose: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ChessRequest {
    CurrentBoard,
    CurrentTotalMoves,
    CurrentOutcome,
    // MovePiece { source: Square, destination: Square }, // TODO: See if can turn into BitMove
    MovePiece { bit_move: BitMoveWrapper }, // TODO: See if can turn into BitMove
    Abort { message: String },
    UndoMoves { moves: u16 },
}

impl ChessRequest {
    /// Is a spectator allowed to send this request
    pub fn available_to_spectator(&self) -> bool {
        match self {
            ChessRequest::CurrentBoard | ChessRequest::CurrentTotalMoves => true,
            _ => false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ChessUpdate {
    /// Usually the Response to `ChessRequest::CurrentBoard`.
    /// But may be sent at other points to synchronize state as well.
    Board {
        movelist: Option<Vec<BitMoveWrapper>>,
    },
    PlayerMove {
        player: Player, // Player who made the move
        last_move: BitMoveWrapper,
        moves_played: u16, // Before player made a move
    },
    MovePieceFailedResponse {
        // Response to `ChessRequest::MovePiece` when the action failed
        message: String,
        rollback_move: BitMoveWrapper,
    },
    Outcome {
        outcome: Option<ChessOutcome>,
    },
    PossibleMoves {
        possible_moves: Vec<(Square /* From */, Square /* To */)>,
    },
    /// Something went wrong and the server wants to tell you about it
    GenericErrorResponse {
        message: String,
    },
    UndoMovesFailedResponse {
        message: String,
    },
    MovesUndone {
        who: Player,
        moves: u16,
    },
    CurrentTotalMovesReponse {
        total_moves: u16,
    },
}

pub async fn create_game(
    white: (Sender<ChessUpdate>, Receiver<ChessRequest>),
    black: (Sender<ChessUpdate>, Receiver<ChessRequest>),
    spectators: (Sender<ChessUpdate>, Receiver<ChessRequest>),
    config: ChessConfig,
) -> Result<()> {
    let mut game = if let Some(ref fen) = config.starting_fen {
        todo!(); // Remove this
        ChessGame::from_fen(fen)?
    } else {
        ChessGame::default()
    };

    let (mut white_tx, mut white_rx) = white;
    let (mut black_tx, mut black_rx) = black;
    let (mut spectators_tx, mut spectators_rx) = spectators;

    let (combined_tx, mut combined_rx) = channel::<(Option<Player>, ChessRequest)>(1024);

    macro_rules! send_to_everyone {
        ($msg: expr) => {
            white_tx.send($msg.clone()).await.ok();
            black_tx.send($msg.clone()).await.ok();
            spectators_tx.send($msg).await.ok();
        };
    }

    // Redirect all rx streams into `combined_rx` with a supplied player for cleaner handling
    // TODO: Shorten/cleanup code
    let mut combined_white_tx = combined_tx.clone();
    task::spawn(async move {
        let player = Some(Player::White);
        loop {
            let update = match white_rx.next().await {
                Some(update) => update,
                None => {
                    combined_white_tx
                        .send((
                            player,
                            ChessRequest::Abort {
                                message: "[Internal] Connection lost".to_owned(),
                            },
                        ))
                        .await
                        .ok();
                    return;
                }
            };
            if let Err(_) = combined_white_tx.send((player, update)).await {
                return;
            }
        }
    });
    let mut combined_black_tx = combined_tx.clone();
    task::spawn(async move {
        loop {
            let update = match black_rx.next().await {
                Some(update) => update,
                None => return,
            };
            if let Err(_) = combined_black_tx.send((Some(Player::Black), update)).await {
                return;
            }
        }
    });
    let mut combined_spectators_tx = combined_tx;
    task::spawn(async move {
        loop {
            let update = match spectators_rx.next().await {
                Some(update) => update,
                None => return,
            };
            if let Err(_) = combined_spectators_tx.send((None, update)).await {
                return;
            }
        }
    });

    // Start (if not using a FEN then white starts)
    todo!("Figure out how to handle FENs or maybe remove them altogether?");
    send_to_everyone!(ChessUpdate::Board {
        movelist: Some(vec![])
    });
    // Send the starting player his possible moves
    let possible_moves: Vec<_> = game
        .possible_moves()
        .iter()
        .map(|bit_move| (bit_move.get_src().into(), bit_move.get_dest().into()))
        .collect();
    match game.turn() {
        PlecoPlayer::White => white_tx.clone(),
        PlecoPlayer::Black => black_tx.clone(),
    }
    .send(ChessUpdate::PossibleMoves { possible_moves })
    .await
    .ok();

    info!("Game initialized. Handling requests...");

    // Handle inputs
    loop {
        let (sender, request): (Option<Player>, ChessRequest) = match combined_rx.next().await {
            Some(res) => res,
            None => {
                break; // No senders connected anymore
            }
        };

        if sender.is_none() && !request.available_to_spectator() {
            spectators_tx
                .send(ChessUpdate::GenericErrorResponse {
                    message: "Spectators can't send this kind of request!".to_owned(),
                })
                .await
                .ok();
            continue;
        }

        macro_rules! send_to_sender {
            ($msg: expr) => {
                match sender {
                    Some(player) => match player {
                        Player::White => white_tx.send($msg).await.ok(),
                        Player::Black => black_tx.send($msg).await.ok(),
                    },
                    None => spectators_tx.send($msg).await.ok(),
                };
            };
        }

        macro_rules! send_to_other_player {
            ($msg: expr) => {
                match sender.context("Send to the other player")? {
                    Player::White => black_tx.send($msg).await.ok(),
                    Player::Black => white_tx.send($msg).await.ok(),
                };
            };
        }

        // Requests that players as well as spectators can send
        match request {
            ChessRequest::CurrentBoard => {
                send_to_sender!(ChessUpdate::Board {
                    movelist: Some(game.movelist().into()),
                });
            }
            ChessRequest::CurrentTotalMoves => {
                send_to_sender!(ChessUpdate::CurrentTotalMovesReponse {
                    total_moves: game.total_moves()
                });
            }
            ChessRequest::CurrentOutcome => {
                send_to_sender!(ChessUpdate::Outcome {
                    outcome: game.outcome()
                });
            }
            _ => {} // Should be handles for a player request
        }

        // Requests that only players can send
        let sender = sender
            .context("available_to_spectator() is probably not up to date with the handlers (message that has to be playerspecific was sent from a spectator)!!!")?;
        match request {
            ChessRequest::MovePiece { bit_move } => {
                println!("moving piece");
                let prev_outcome = game.outcome();
                let prev_moves_played = game.total_moves();
                match game.move_piece(bit_move.into()) {
                    Ok(_) => {
                        send_to_everyone!(ChessUpdate::PlayerMove {
                            player: sender,
                            last_move: bit_move,
                            moves_played: prev_moves_played,
                        });
                        let new_outcome = game.outcome();
                        if prev_outcome != new_outcome {
                            send_to_everyone!(ChessUpdate::Outcome {
                                outcome: new_outcome
                            });
                        }

                        if new_outcome.is_none() {
                            // Send possible moves to player
                            send_to_other_player!(ChessUpdate::PossibleMoves {
                                possible_moves: game
                                    .possible_moves()
                                    .iter()
                                    .map(|bit_move| (
                                        bit_move.get_src().into(),
                                        bit_move.get_dest().into()
                                    ))
                                    .collect(),
                            });
                        }
                    }
                    Err(e) => {
                        send_to_sender!(ChessUpdate::MovePieceFailedResponse {
                            message: format!("Denied by engine: {}", e),
                            rollback_move: bit_move,
                        });
                    }
                };
            }
            ChessRequest::Abort { .. /* message */ } => {
                game.player_left(sender);
                break;
            },
            ChessRequest::UndoMoves { moves } => {
                let player_allowed = match sender {
                    Player::Black => config.can_black_undo,
                    Player::White => config.can_white_undo,
                };
                if ! player_allowed {
                    send_to_sender!(ChessUpdate::UndoMovesFailedResponse {
                        message: "You are not permitted to do that in this game.".to_owned(),
                    });
                } else if !(game.turn() == sender && game.outcome().is_none() || game.outcome().is_some() && config.allow_undo_after_loose) {
                    if config.allow_undo_after_loose {
                        send_to_sender!(ChessUpdate::UndoMovesFailedResponse {
                            message: "You can only undo when you are playing or it's game over.".to_owned(),
                        });
                    }else {
                        send_to_sender!(ChessUpdate::UndoMovesFailedResponse {
                            message: "You can only undo when you are playing.".to_owned(),
                    });
                    }
                }else {
                    let prev_outcome = game.outcome();
                    if let Err(e) = game.undo(moves) {
                        send_to_sender!(ChessUpdate::UndoMovesFailedResponse {
                            message: format!("Denied by engine: {}", e),
                        });
                    }else {
                        let new_outcome = game.outcome();
                        if prev_outcome != new_outcome {
                            send_to_everyone!(ChessUpdate::Outcome {
                                outcome: new_outcome
                            });
                        }
                        todo!("Handle undos");
                        // // Select current player and update board
                        // send_to_everyone!(ChessUpdate::PlayerSwitch {
                        //     player: game.turn(),
                        //     last_move: game.last_move(),
                        //     moves_played: game.total_moves(),
                        // });
                        // Send the starting player his possible moves
                        let possible_moves: Vec<_> = game
                            .possible_moves()
                            .iter()
                            .map(|bit_move| (bit_move.get_src().into(), bit_move.get_dest().into()))
                            .collect();
                        match game.turn() {
                            PlecoPlayer::White => white_tx.clone(),
                            PlecoPlayer::Black => black_tx.clone(),
                        }
                        .send(ChessUpdate::PossibleMoves { possible_moves })
                        .await
                        .ok();
                        // Notify everyone of undo
                        send_to_everyone!(ChessUpdate::MovesUndone {
                            who: sender,
                            moves,
                        });

                    }
                }
            }
            _ => {
                bail!("available_to_spectator() is probably not up to date with the handlers (player specific handler found a unhandled entry)!!!");
            }
        };
    }

    // Potential cleanup here
    info!("Game terminated seemingly gracefully");
    Ok(())
}

pub async fn create_bot<T: Searcher>(
    me: Player,
    depth: u16,
    min_reaction_delay: Duration,
) -> Result<(Sender<ChessUpdate>, Receiver<ChessRequest>)> {
    let (update_tx, mut update_rx) = channel::<ChessUpdate>(256);
    let (mut request_tx, request_rx) = channel::<ChessRequest>(256);

    task::spawn(async move {
        info!("Bot spawned for {}", me);
        let mut current_outcome: Option<ChessOutcome> = None;
        let mut board = pleco::Board::default();
        while let Some(update) = update_rx.recv().await {
            match update {
                ChessUpdate::Board { movelist } => {
                    board = pleco::Board::default();
                    match movelist {
                        None => {}
                        Some(movelist) => {
                            for single_move in movelist.into_iter() {
                                board.apply_move(single_move.into());
                            }
                        }
                    }
                }
                ChessUpdate::PlayerMove {
                    player,
                    last_move,
                    moves_played,
                } => {
                    if player == me && current_outcome.is_none() {
                        assert!(
                            board.turn() == player.into() && board.moves_played() == moves_played,
                            "Turn desynchronized."
                        );

                        board.apply_move(last_move.into());

                        let board_copy = pleco::Board::from_fen(&*board.fen()).unwrap();

                        let bit_move = task::spawn_blocking(move || {
                            let started = SystemTime::now();
                            let bit_move = T::best_move(board_copy, depth);
                            let elapsed = started.elapsed().unwrap_or(Duration::new(0, 0));

                            if elapsed < min_reaction_delay {
                                thread::sleep(min_reaction_delay - elapsed);
                            } else {
                                info!("Bot took a long time to think: {:?}", elapsed);
                            }
                            bit_move
                        })
                        .await
                        .context("Blocking heavy calculation")
                        .unwrap();

                        request_tx
                            .send(ChessRequest::MovePiece {
                                bit_move: bit_move.into()
                            })
                            .await
                            .expect("Bot failed to send move");
                    }
                }
                ChessUpdate::MovePieceFailedResponse { message, .. } => {
                    error!("A move from the bot was rejected: {}", message);
                    break;
                }
                ChessUpdate::Outcome { outcome } => {
                    if outcome.is_some() {
                        info!("Bot detected that the game ended");
                    //break;
                    } else {
                        info!("Game continues. Bot will continue playing.");
                    }
                    current_outcome = outcome;
                }
                _ => {}
            }
        }
        info!("Bot task has ended");
    });

    Ok((update_tx, request_rx))
}

pub fn stubbed_spectator() -> (Sender<ChessUpdate>, Receiver<ChessRequest>) {
    // Channel size doesn't matter since the channels are closed after this
    // function returns since one side of each channel gets dropped at that point.
    let (update_tx, _) = channel::<ChessUpdate>(1);
    let (_, request_rx) = channel::<ChessRequest>(1);
    (update_tx, request_rx)
}
