use crate::base::behavior::*;
use crate::dpi::CELL;

#[no_mangle]
pub unsafe extern "C" fn cyclotron_tile_tick_rs(
) {
    let mut context_guard = CELL.write().unwrap();
    let context = context_guard.as_mut().expect("DPI context not initialized!");
    let sim = &mut context.sim_isa;
    let config = sim.top.clusters[0].cores[0].conf().clone();

    println!("tick: {}", config.num_lanes);
}
