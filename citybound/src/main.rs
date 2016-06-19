#![allow(dead_code)]
extern crate world_record;
extern crate monet;

use std::path::PathBuf;
use std::thread;
use std::sync::mpsc::channel;
use monet::glium::DisplayBuild;
use monet::glium::glutin;

mod models;
mod steps;
mod simulation;
#[path = "../resources/car.rs"]
mod car;

fn main() {
    let (to_simulation, from_renderer) = channel::<()>();
    let (to_renderer, from_simulation) = channel::<monet::Scene>();
    
    let renderer_listener = move |past: &models::State, future: &models::State| {
        match from_renderer.try_recv() {
            Ok(_) => {
                println!("creating renderer state...");
                let mut scene = monet::Scene::new();
                scene.things.insert("car", car::create());
                scene.debug_text = format!("Simulation frame: {}", past.core.header.ticks);
                to_renderer.send(scene).unwrap();
            },
            Err(_) => {}
        };
        
    };
    
    thread::Builder::new().name("simulation".to_string()).spawn(|| {
        let mut simulation = simulation::Simulation::<models::State>::new(
            PathBuf::from("savegames/dev"),
            vec! [Box::new(steps::tick)],
            vec! [Box::new(renderer_listener)]
        );
    
       loop {
           let duration_to_sleep = simulation.step();
           thread::sleep(duration_to_sleep);
       }
    }).unwrap();
    
    let window = glutin::WindowBuilder::new()
        .with_title("Citybound".to_string())
        .with_dimensions(512, 512)
        .with_multitouch()
        .with_vsync().build_glium().unwrap();

    let renderer = monet::Renderer::new(&window);

    'main: loop {
        // loop over events
        for event in window.poll_events() {
            match event {
                glutin::Event::KeyboardInput(_, _, Some(glutin::VirtualKeyCode::Escape)) |
                glutin::Event::Closed => break 'main,
                _ => {},
            }
        }
        
        to_simulation.send(()).unwrap();
        let scene = from_simulation.recv().unwrap();
        println!("rendering...");

        renderer.draw(scene)
    }
}