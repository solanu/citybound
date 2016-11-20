pub mod lane_rendering;
pub mod lane_thing_collector;
pub mod planning;
mod intelligent_acceleration;
use self::intelligent_acceleration::{intelligent_acceleration, COMFORTABLE_BREAKING_DECELERATION};
use core::geometry::CPath;
use kay::{ID, Actor, CVec, Swarm, CreateWith, Recipient, ActorSystem, Fate};
use descartes::{FiniteCurve, RoughlyComparable, Band, Intersect, Curve};
use ordered_float::OrderedFloat;
use itertools::Itertools;
use ::std::f32::INFINITY;
use ::std::ops::{Deref, DerefMut};

#[derive(Compact, Actor, Clone)]
pub struct Lane {
    _id: ID,
    length: f32,
    path: CPath,
    interactions: CVec<Interaction>,
    interaction_obstacles: CVec<Obstacle>,
    cars: CVec<LaneCar>,
    in_construction: f32
}

impl Lane {
    pub fn new(path: CPath) -> Self {
        Lane {
            _id: ID::invalid(),
            length: path.length(),
            path: path,
            interactions: CVec::new(),
            interaction_obstacles: CVec::new(),
            cars: CVec::new(),
            in_construction: 0.0
        }
    }
}

#[derive(Compact, Actor, Clone)]
pub struct TransferLane {
    _id: ID,
    length: f32,
    path: CPath,
    left: ID,
    left_start: f32,
    right: ID,
    right_start: f32,
    interaction_obstacles: CVec<Obstacle>,
    cars: CVec<TransferringLaneCar>
}

impl TransferLane {
    fn new(path: CPath, left: ID, left_start: f32, right: ID, right_start: f32) -> TransferLane {
        TransferLane{
            _id: ID::invalid(),
            length: path.length(),
            path: path,
            left: left,
            left_start: left_start,
            right: right,
            right_start: right_start,
            interaction_obstacles: CVec::new(),
            cars: CVec::new()
        }
    }
}

#[derive(Copy, Clone)]
enum Add{
    Car(LaneCar),
    InteractionObstacle(Obstacle)
}

impl Recipient<Add> for Lane {
    fn receive(&mut self, msg: &Add) -> Fate {match *msg{
        Add::Car(car) => {
            // TODO: optimize using BinaryHeap?
            self.cars.push(car);
            self.cars.sort_by_key(|car| car.as_obstacle.position);
            Fate::Live
        },
        Add::InteractionObstacle(obstacle) => {
            self.interaction_obstacles.push(obstacle);
            Fate::Live
        }
    }}
}

impl Recipient<Add> for TransferLane {
    fn receive(&mut self, msg: &Add) -> Fate {match *msg{
        Add::Car(car) => {
            self.cars.push(TransferringLaneCar{
                as_lane_car: car,
                transfer_position: -1.0,
                transfer_velocity: 0.0,
                transfer_acceleration: 0.1
            });
            // TODO: optimize using BinaryHeap?
            self.cars.sort_by_key(|car| car.as_obstacle.position);
            Fate::Live
        },
        Add::InteractionObstacle(obstacle) => {
            self.interaction_obstacles.push(obstacle);
            Fate::Live
        },
    }}
}

use core::simulation::Tick;

const TRAFFIC_LOGIC_THROTTLING : usize = 60;

impl Recipient<Tick> for Lane {
    fn receive(&mut self, msg: &Tick) -> Fate {match *msg{
        Tick{dt, current_tick} => {
            self.in_construction += dt * 400.0;

            let do_traffic = current_tick % TRAFFIC_LOGIC_THROTTLING == self.id().instance_id as usize % TRAFFIC_LOGIC_THROTTLING;

            if do_traffic {
                // TODO: optimize using BinaryHeap?
                self.interaction_obstacles.sort_by_key(|obstacle| obstacle.position);

                let mut overlap_obstacles = self.interaction_obstacles.iter();
                let mut maybe_next_overlap_obstacle = overlap_obstacles.next();

                for c in 0..self.cars.len() {
                    let next_obstacle = self.cars.get(c + 1).map_or(Obstacle::far_ahead(), |car| car.as_obstacle);
                    let car = &mut self.cars[c];
                    let next_obstacle_acceleration = intelligent_acceleration(car, &next_obstacle);
                    
                    maybe_next_overlap_obstacle = maybe_next_overlap_obstacle.and_then(|obstacle| {
                        let mut following_obstacle = Some(obstacle);
                        while following_obstacle.is_some() && following_obstacle.unwrap().position < car.position {
                            following_obstacle = overlap_obstacles.next();
                        }
                        following_obstacle
                    });
                    
                    let next_overlap_obstacle_acceleration = if let Some(next_overlap_obstacle) = maybe_next_overlap_obstacle {
                        intelligent_acceleration(car, next_overlap_obstacle)
                    } else {INFINITY};

                    car.acceleration = next_obstacle_acceleration.min(next_overlap_obstacle_acceleration);
                }
            }
            if do_traffic {
                self.interaction_obstacles.clear();
            }

            for car in &mut self.cars {
                *car.position += dt * car.velocity;
                car.velocity = (car.velocity + dt * car.acceleration).min(car.max_velocity).max(0.0);
            }
            
            loop {
                let should_pop = self.cars.iter().rev().find(|car| *car.position > self.length).map(|car_over_end| {
                    let first_next_interaction = self.interactions.iter().find(|interaction| match interaction.kind {Next{..} => true, _ => false});
                    if let Some(&Interaction{partner_lane, kind: Next{partner_start}, ..}) = first_next_interaction {
                        partner_lane << Add::Car(car_over_end.offset_by(-self.length + partner_start));
                    };
                    car_over_end
                }).is_some();
                if should_pop {self.cars.pop();} else {break;}
            }

            for interaction in self.interactions.iter() {
                let mut cars = self.cars.iter();
                let send_obstacle = |obstacle: Obstacle| interaction.partner_lane << Add::InteractionObstacle(obstacle);
                
                if current_tick % TRAFFIC_LOGIC_THROTTLING == interaction.partner_lane.instance_id as usize % TRAFFIC_LOGIC_THROTTLING {

                    match interaction.kind {
                        Overlap{start, end, partner_start, kind, ..} => {
                            match kind {
                                Parallel => cars.skip_while(|car: &&LaneCar| *car.position < start)
                                                .take_while(|car: &&LaneCar| *car.position < end)
                                                .map(|car| car.as_obstacle.offset_by(-start + partner_start)
                                            ).foreach(send_obstacle),
                                Conflicting => if cars.any(|car: &LaneCar| *car.position > start && *car.position < end) {
                                    (send_obstacle)(Obstacle{position: OrderedFloat(partner_start), velocity: 0.0, max_velocity: 0.0})
                                }
                            }
                        }
                        Previous{start, partner_length} =>
                            if let Some(next_car) = cars.find(|car| *car.position > start) {
                                (send_obstacle)(next_car.as_obstacle.offset_by(-start + partner_length))
                            },
                        Next{..} => {
                            //TODO: for looking backwards for merging lanes?
                        }
                    };
                }
            }
            Fate::Live
        }
    }}
}

impl Recipient<Tick> for TransferLane {
    fn receive(&mut self, msg: &Tick) -> Fate {match *msg{
        Tick{dt, ..} => {
            self.interaction_obstacles.sort_by_key(|obstacle| obstacle.position);

            for c in 0..self.cars.len() {
                let (acceleration, is_dangerous) = {
                    let car = &self.cars[c];
                    
                    let next_obstacle = self.cars.get(c + 1).map_or(Obstacle::far_ahead(), |car| car.as_obstacle);
                    let previous_obstacle = if c > 0 {self.cars[c - 1].as_obstacle} else {Obstacle::far_behind()};

                    let next_interaction_obstacle_index = self.interaction_obstacles.iter().position(
                        |obstacle| obstacle.position > car.position
                    );
                    let next_interaction_obstacle = next_interaction_obstacle_index
                        .map(|idx| self.interaction_obstacles[idx]).unwrap_or_else(Obstacle::far_ahead);
                    let previous_interaction_obstacle = next_interaction_obstacle_index
                        .and_then(|idx| self.interaction_obstacles.get(idx - 1)).cloned().unwrap_or_else(Obstacle::far_behind);

                    let next_obstacle_acceleration = intelligent_acceleration(car, &next_obstacle)
                        .min(intelligent_acceleration(car, &next_interaction_obstacle));
                    let previous_obstacle_acceleration = intelligent_acceleration(&previous_obstacle, &car.as_obstacle)
                        .min(intelligent_acceleration(&previous_interaction_obstacle, &car.as_obstacle));

                    let politeness_factor = 0.3;

                    let acceleration = if previous_obstacle_acceleration < 0.0 {
                        (1.0 - politeness_factor) * next_obstacle_acceleration + politeness_factor * previous_obstacle_acceleration
                    } else {
                        next_obstacle_acceleration
                    };

                    let is_dangerous = next_obstacle_acceleration < -2.0 * COMFORTABLE_BREAKING_DECELERATION
                        || previous_obstacle_acceleration < -2.0 * COMFORTABLE_BREAKING_DECELERATION;

                    (acceleration, is_dangerous)
                };

                let car = &mut self.cars[c];
                car.acceleration = acceleration;
                if is_dangerous {
                    car.transfer_acceleration = if car.transfer_position >= 0.0 {0.3} else {-0.3}
                }
                // smooth out arrival on other lane
                #[allow(float_cmp)]
                let arriving_soon = car.transfer_velocity.abs() > 0.1 && car.transfer_position.abs() > 0.5 && car.transfer_position.signum() == car.transfer_velocity.signum();
                if arriving_soon {
                    car.transfer_acceleration = -0.9 * car.transfer_velocity;
                }
            }

            for car in &mut self.cars {
                *car.position += dt * car.velocity;
                car.velocity = (car.velocity + dt * car.acceleration).min(car.max_velocity).max(0.0);
                car.transfer_position += dt * car.transfer_velocity;
                car.transfer_velocity += dt * car.transfer_acceleration;
                if car.transfer_velocity.abs() > car.velocity/12.0 {
                    car.transfer_velocity = car.velocity/12.0 * car.transfer_velocity.signum();
                }
            }

            let mut i = 0;
            loop {
                let (should_remove, done) = if let Some(car) = self.cars.get(i) {
                    if car.transfer_position < -1.0 {
                        self.left << Add::Car(car.as_lane_car.offset_by(self.left_start));
                        (true, false)
                    } else if car.transfer_position > 1.0 {
                        self.right << Add::Car(car.as_lane_car.offset_by(self.right_start));
                        (true, false)
                    } else {
                        i += 1;
                        (false, false)
                    }
                } else {
                    (false, true)
                };
                if done {break;}
                if should_remove {self.cars.remove(i);}
            }

            for car in &self.cars {
                if car.transfer_position < 0.3 || car.transfer_velocity < 0.0 {
                    self.left << Add::InteractionObstacle(car.as_obstacle.offset_by(self.left_start));
                }
                if car.transfer_position > -0.3 || car.transfer_velocity > 0.0 {
                    self.right << Add::InteractionObstacle(car.as_obstacle.offset_by(self.right_start));
                }
            }

            self.interaction_obstacles.clear();
            Fate::Live
        }
    }}
}

use self::planning::materialized_reality::BuildableRef;

#[derive(Copy, Clone)]
pub struct AdvertiseForConnectionAndReport(ID, BuildableRef);

#[derive(Compact, Clone)]
pub struct Connect{other_id: ID, other_path: CPath, reply_needed: bool}

use self::planning::materialized_reality::ReportLaneBuilt;

impl Recipient<AdvertiseForConnectionAndReport> for Lane {
    fn receive(&mut self, msg: &AdvertiseForConnectionAndReport) -> Fate {match *msg{
        AdvertiseForConnectionAndReport(report_to, report_as) => {
            Swarm::<Lane>::all() << Connect{
                other_id: self.id(),
                other_path: self.path.clone(),
                reply_needed: true
            };
            report_to << ReportLaneBuilt(self.id(), report_as);
            self::lane_rendering::on_build(self);
            Fate::Live
        }
    }}
}

const CONNECTION_TOLERANCE: f32 = 0.1;

impl Recipient<Connect> for Lane {
    fn receive(&mut self, msg: &Connect) -> Fate {match *msg{
        Connect{other_id, ref other_path, reply_needed} => {
            if other_id == self.id() {return Fate::Live};

            if other_path.start().is_roughly_within(self.path.end(), CONNECTION_TOLERANCE) {
                self.interactions.push(Interaction{
                    partner_lane: other_id,
                    kind: InteractionKind::Next{
                        partner_start: 0.0
                    }
                })
            } else if let Some(self_end_on_other) = other_path.project(self.path.end()) {
                if other_path.along(self_end_on_other).is_roughly_within(self.path.end(), CONNECTION_TOLERANCE) {
                    self.interactions.push(Interaction{
                        partner_lane: other_id,
                        kind: InteractionKind::Next{
                            partner_start: self_end_on_other
                        }
                    })
                }
            }

            if other_path.end().is_roughly_within(self.path.start(), CONNECTION_TOLERANCE) {
                self.interactions.push(Interaction{
                    partner_lane: other_id,
                    kind: InteractionKind::Previous{
                        start: 0.0,
                        partner_length: other_path.length()
                    }
                })
            } else if let Some(other_end_on_self) = self.path.project(other_path.end()) {
                if self.path.along(other_end_on_self).is_roughly_within(other_path.end(), CONNECTION_TOLERANCE) {
                    self.interactions.push(Interaction{
                        partner_lane: other_id,
                        kind: InteractionKind::Previous{
                            start: other_end_on_self,
                            partner_length: other_path.length()
                        }
                    })
                }
            }

            let self_band = Band::new(self.path.clone(), 5.0);
            let other_band = Band::new(other_path.clone(), 5.0);
            let intersections = (&self_band.outline(), &other_band.outline()).intersect();
            if intersections.len() >= 2 {
                let (entry_intersection, entry_distance) = intersections.iter().map(
                    |intersection| (intersection, self_band.outline_distance_to_path_distance(intersection.along_a))
                ).min_by_key(
                    |&(_, distance)| OrderedFloat(distance)
                ).expect("entry should exist");

                let (exit_intersection, exit_distance) = intersections.iter().map(
                    |intersection| (intersection, self_band.outline_distance_to_path_distance(intersection.along_a))
                ).max_by_key(
                    |&(_, distance)| OrderedFloat(distance)
                ).expect("exit should exist");

                let other_entry_distance = other_band.outline_distance_to_path_distance(entry_intersection.along_b);
                let other_exit_distance = other_band.outline_distance_to_path_distance(exit_intersection.along_b);

                self.interactions.push(Interaction{
                    partner_lane: other_id,
                    kind: if other_entry_distance < other_exit_distance {
                        InteractionKind::Overlap{
                            start: entry_distance,
                            end: exit_distance,
                            partner_start: other_entry_distance,
                            partner_end: other_exit_distance,
                            kind: OverlapKind::Parallel
                        }
                    } else {
                        InteractionKind::Overlap{
                            start: entry_distance,
                            end: exit_distance,
                            partner_start: other_exit_distance,
                            partner_end: other_entry_distance,
                            kind: OverlapKind::Conflicting
                        }
                    }
                });
            }

            if reply_needed {
                other_id << Connect{
                    other_id: self.id(),
                    other_path: self.path.clone(),
                    reply_needed: false
                };
            }
            Fate::Live
        }
    }}
}

#[derive(Copy, Clone)]
pub struct Disconnect{other_id: ID}

impl Recipient<Disconnect> for Lane {
    fn receive(&mut self, msg: &Disconnect) -> Fate {match *msg{
        Disconnect{other_id} => {
            // TODO: use retain
            self.interactions = self.interactions.iter().filter(|interaction|
                interaction.partner_lane != other_id
            ).cloned().collect();
            Fate::Live
        }
    }}
}

#[derive(Copy, Clone)]
pub struct Unbuild;

impl Recipient<Unbuild> for Lane{
    fn receive(&mut self, _msg: &Unbuild) -> Fate {
        Swarm::<Lane>::all() << Disconnect{other_id: self.id()}; 
        self::lane_rendering::on_unbuild(self);
        Fate::Die
    }
}

pub fn setup(system: &mut ActorSystem) {
    system.add_individual(Swarm::<Lane>::new());
    system.add_inbox::<CreateWith<Lane, AdvertiseForConnectionAndReport>, Swarm<Lane>>();
    system.add_inbox::<Add, Swarm<Lane>>();
    system.add_inbox::<Tick, Swarm<Lane>>();
    system.add_inbox::<Connect, Swarm<Lane>>();
    system.add_inbox::<Disconnect, Swarm<Lane>>();
    system.add_inbox::<Unbuild, Swarm<Lane>>();

    system.add_individual(Swarm::<TransferLane>::new());
    system.add_inbox::<Add, Swarm<TransferLane>>();
    system.add_inbox::<Tick, Swarm<TransferLane>>();
}

#[derive(Copy, Clone)]
pub struct Obstacle {
    position: OrderedFloat<f32>,
    velocity: f32,
    max_velocity: f32
}

impl Obstacle {
    fn far_ahead() -> Obstacle {Obstacle{position: OrderedFloat(INFINITY), velocity: INFINITY, max_velocity: INFINITY}}
    fn far_behind() -> Obstacle {Obstacle{position: OrderedFloat(-INFINITY), velocity: 0.0, max_velocity: 20.0}}
    fn offset_by(&self, delta: f32) -> Obstacle {
        Obstacle{
            position: OrderedFloat(*self.position + delta),
            .. *self
        }
    } 
}

#[derive(Copy, Clone)]
pub struct LaneCar {
    trip: ID,
    as_obstacle: Obstacle,
    acceleration: f32
}

impl LaneCar {
    fn offset_by(&self, delta: f32) -> LaneCar {
        LaneCar{
            as_obstacle: self.as_obstacle.offset_by(delta),
            .. *self
        }
    }
}

impl Deref for LaneCar {
    type Target = Obstacle;

    fn deref(&self) -> &Obstacle {&self.as_obstacle}
}

impl DerefMut for LaneCar {
    fn deref_mut(&mut self) -> &mut Obstacle {&mut self.as_obstacle}
}

#[derive(Copy, Clone)]
struct TransferringLaneCar {
    as_lane_car: LaneCar,
    transfer_position: f32,
    transfer_velocity: f32,
    transfer_acceleration: f32
}

impl Deref for TransferringLaneCar {
    type Target = LaneCar;

    fn deref(&self) -> &LaneCar {
        &self.as_lane_car
    }
}

impl DerefMut for TransferringLaneCar {
    fn deref_mut(&mut self) -> &mut LaneCar {
        &mut self.as_lane_car
    }
}

#[derive(Copy, Clone)]
struct Interaction {
    partner_lane: ID,
    kind: InteractionKind
}

#[derive(Copy, Clone)]
enum InteractionKind{
    Overlap{
        start: f32,
        end: f32,
        partner_start: f32,
        partner_end: f32,
        kind: OverlapKind
    },
    Next{
        partner_start: f32
    },
    Previous{
        start: f32,
        partner_length: f32
    }
}
use self::InteractionKind::{Overlap, Next, Previous};

#[derive(Copy, Clone)]
enum OverlapKind{Parallel, Conflicting}
use self::OverlapKind::{Parallel, Conflicting};