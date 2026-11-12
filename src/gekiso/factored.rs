//! Score-only ordinary skills leave the Gekiso judgement and LIFE stream unchanged.
use super::*;
use crate::{live::{Effect,Gate},master::SkillEffect,num::{judgement_delta,score_delta}};

#[derive(PartialEq,Eq)]
struct Key {tables:[usize;12],deck:Vec<Performer>,seeds:Vec<i32>,ranks:Vec<i32>,music_type:i32}
struct Entry {key:Key,context:Arc<Context>,bytes:usize,_master:Master}
#[derive(Default)]
pub(super) struct Cache {entries:std::collections::VecDeque<Entry>,bytes:usize,frame_ms:Option<i32>,just:Option<(Arc<crate::master::LiveTables>,Context)>}
pub(super) fn just_context(master:&Master,chart:&Chart,deck:&[Performer])->Option<Context>{
    if chart.setup.missions.iter().any(|&mission|mission!=3){return None;}
    for actor in deck{
        let rows=actor.gekisou_skill.into_iter().filter_map(|(id,level)|master.gekiso_skills.get(&id).map(|skill|(skill,level)))
            .chain(actor.gekisou_support_skills.iter().filter_map(|&(id,level)|master.gekiso_support_skills.get(&id).map(|skill|(skill,level))))
            .filter(|(skill,_)|skill.mission_type==4||skill.mission_type==3)
            .flat_map(|(skill,level)|skill.effects.iter().filter(move|row|row.level==level));
        if rows.into_iter().any(|row|!matches!(row.kind,13000|13002|13003|13004|4004)){return None;}
    }
    if replay_random_skills(master,chart,deck){return None;}
    let mut cache=chart.factored.lock().unwrap();
    if let Some((live,context))=&cache.just{if Arc::ptr_eq(live,&master.live){return Some(context.clone());}}
    let n=chart.notes.len();let mut life=crate::full::core::Life::new(master.live.life_base as i32,master.live.damage,chart.params().music_length);
    let notes:HashMap<_,_>=chart.full.notes.iter().map(|note|(note.id,note)).collect();
    let scored:HashMap<_,_>=chart.notes.iter().enumerate().map(|(index,note)|(note.id,index)).collect();
    let mut judgement=chart.judgements.clone();let mut lives=vec![master.live.life_base as i32;n];
    let mut event_life=vec![master.live.life_base as i32;chart.full.events.len()];
    let mut firing:Vec<_>=(0..chart.full.events.len()).collect();firing.sort_by_key(|&index|(chart.full.events[index].1,index));let mut event=0;
    for frame in &chart.play.frames{
        for judged in &frame.judged{
            let note=notes[&judged.note_id];life.note_damage(note.time_ms,judged.judgement as usize);
            if let Some(&index)=scored.get(&note.id){lives[index]=life.at(note.time_ms);judgement[index]=judged.judgement as usize;}
        }
        while event<firing.len()&&chart.full.events[firing[event]].1<=frame.time_ms{event_life[firing[event]]=life.at(frame.time_ms);event+=1;}
        life.sync(frame.time_ms);
    }
    let context=Context{life:Some(Arc::new(lives)),live_life:Some(Arc::new(event_life)),judgement,gk:vec![1.0;n],up:vec![1.0;n],luck:vec![1.0;n],seeds:1,
        factor_commands:0,factor_peak:0.0,rush_changes:0,rush_trace:None,draws:Vec::new(),nominal:None};
    if context.retained_bytes()<=2*1024*1024{cache.just=Some((Arc::clone(&master.live),context.clone()));}
    Some(context)
}
fn context(master:&Master,chart:&Chart,deck:&[Performer],seeds:&[i32])->Result<Arc<Context>,String> {
    let mut reduced:Vec<_>=deck.iter().map(Performer::gekiso_only).collect();
    for actor in &mut reduced {actor.gekisou_support_skills.retain(|&(id,_)|master.gekiso_support_skills.get(&id).is_some_and(|skill|skill.mission_type==4||chart.setup.missions.contains(&skill.mission_type)));}
    let key=Key {tables:[Arc::as_ptr(&master.live) as usize,Arc::as_ptr(&master.params) as usize,
        Arc::as_ptr(&master.live_skills) as usize,Arc::as_ptr(&master.support_skills) as usize,
        Arc::as_ptr(&master.gekiso_skills) as usize,Arc::as_ptr(&master.gekiso_support_skills) as usize,
        Arc::as_ptr(&master.conditions) as usize,Arc::as_ptr(&master.condition_sets) as usize,Arc::as_ptr(&master.targets) as usize,
        Arc::as_ptr(&master.cumulative) as usize,Arc::as_ptr(&master.live_categories) as usize,Arc::as_ptr(&master.gekiso_categories) as usize],
        deck:reduced,seeds:seeds.to_vec(),ranks:chart.setup.ranks.clone(),music_type:chart.music_type};
    {
        let mut cache=chart.factored.lock().unwrap();
        if let Some(index)=cache.entries.iter().position(|entry|entry.key==key) {
            let entry=cache.entries.remove(index).unwrap();let context=entry.context.clone();cache.entries.push_back(entry);return Ok(context);
        }
    }
    let context=Arc::new(context_fixed(master,chart,&key.deck,seeds)?);
    let bytes=context.retained_bytes()+std::mem::size_of::<Entry>();
    const LIMIT:usize=2*1024*1024;
    if bytes<=LIMIT {
        let mut cache=chart.factored.lock().unwrap();
        while cache.bytes+bytes>LIMIT||cache.entries.len()>=64 {
            if let Some(entry)=cache.entries.pop_front(){cache.bytes-=entry.bytes;}else{break;}
        }
        cache.bytes+=bytes;cache.entries.push_back(Entry{key,context:context.clone(),bytes,_master:master.clone()});
    }
    Ok(context)
}

fn owner(master:&Master,group:i32,actor:&Performer)->Option<bool> {
    if group==0 {return Some(true);}
    let sets=master.condition_sets.get(&group)?;let mut any=false;
    for set in sets {
        let mut all=true;
        for id in set {
            let condition=&master.conditions[id];
            if condition.kind!=5000 {return None;}
            all&=condition.targets.iter().any(|id|performer_matches(actor,&master.targets[id]))==condition.positive;
        }
        any|=all;
    }
    Some(any)
}
fn activation_gate(master:&Master,group:i32,actor:&Performer)->Option<(bool,Gate)> {
    let always=Gate{kind:0,value:0.0,positive:true};
    if group==0{return Some((true,always));}
    let mut unconditional=false;let mut gates=Vec::new();
    for set in master.condition_sets.get(&group)? {
        let mut possible=true;let mut gate=None;
        for id in set {
            let row=&master.conditions[id];
            match row.kind {
                5000=>possible&=row.targets.iter().any(|id|performer_matches(actor,&master.targets[id]))==row.positive,
                2000|2001=>{
                    let value=*row.values.first()?;
                    let next=Gate{kind:1,value:if row.kind==2000{value.floor()+1.0}else{value.ceil()},positive:row.positive};
                    if gate.as_ref().is_some_and(|gate|gate!=&next){return None;}gate=Some(next);
                },
                _=>return None,
            }
        }
        if possible {if let Some(gate)=gate{gates.push(gate);}else{unconditional=true;}}
    }
    if unconditional{return Some((true,always));}
    if let Some(gate)=gates.first(){if gates.iter().any(|other|other!=gate){return None;}return Some((true,gate.clone()));}
    Some((false,always))
}
fn slot(master:&Master,actor:&Performer,minimum_duration:i32)->Option<Slot> {
    let mut effects=Vec::new();let mut extension_ms=0.0;
    if let Some((id,level))=actor.live_skill {
        for row in master.live_skills.get(&id)?.iter().filter(|row|row.level==level) {
            if row.release_group!=0||row.trigger_group!=0||row.reset_group!=0 {return None;}
            let (enabled,gate)=activation_gate(master,row.condition_group,actor)?;
            if !enabled {continue;}
            if gate.kind!=0&&master.live.phases.get(&row.kind)!=Some(&2){return None;}
            if row.kind!=2000||row.value<0||row.release_group!=0||row.trigger_group!=0||row.cumulative_id!=0||row.reset_group!=0||row.limit_count!=0||row.execute_limit!=0 {return None;}
            effects.push(Effect{kind:row.kind,delta:if row.kind==2000 {score_delta(row.value)}else{judgement_delta(row.value)},seconds:f(row.seconds),
                gate,judgements:if row.kind==2004 {row.targets.iter().map(|id|master.targets[id].judgement).filter(|j|(1..=6).contains(j)).map(|j|j as usize).collect()}else{Vec::new()},release_judgements:Vec::new()});
        }
    }
    for &(id,level) in &actor.support_skills {
        for row in master.support_skills.get(&id)?.iter().filter(|row|row.level==level) {
            let live_trigger=row.trigger_group>0&&master.condition_sets.get(&row.trigger_group).is_some_and(|sets|!sets.is_empty()
                &&sets.iter().all(|set|!set.is_empty()&&set.iter().all(|id|matches!(master.conditions[id].kind,4010|5020)&&master.conditions[id].positive)));
            if row.release_group!=0||!live_trigger||row.reset_group!=0 {return None;}
            if !owner(master,row.condition_group,actor)? {continue;}
            if row.kind!=15000||row.release_group!=0||row.reset_group!=0||row.cumulative_id!=0||row.execute_limit!=0||row.limit_count!=0||row.value<0 {return None;}
            extension_ms=f(extension_ms+f(row.value as f64));
        }
    }
    if effects.iter().any(|effect|effect.seconds<=0.0||f(f(effect.seconds*1000.0)+extension_ms)<=minimum_duration as f64){return None;}
    Some(Slot{effects,cumulative:Vec::new(),extension_ms,recovery:0,convert:None})
}
fn independent_row(master:&Master,row:&SkillEffect)->bool {
    if (2000..=2005).contains(&row.kind)||row.kind==15000 {return false;}
    if row.cumulative_id!=0 && master.cumulative.get(&row.cumulative_id).is_none_or(|counter|![1000,1001,1002,7000,7001,7002].contains(&counter.kind)) {return false;}
    [row.condition_group,row.trigger_group,row.release_group,row.reset_group].iter()
        .flat_map(|id|master.condition_sets.get(id).into_iter().flatten().flatten())
        .all(|id|matches!(master.conditions[id].kind,0|1000..=1031|2000|2001|4011|5000|7000..=7021))
}
pub(super) fn seed_actor(master:&Master,chart:&Chart,actor:&Performer)->bool {
    let minimum_duration={let mut cache=chart.factored.lock().unwrap();2 * *cache.frame_ms.get_or_insert_with(||chart.play.frames.windows(2).map(|pair|pair[1].time_ms-pair[0].time_ms).max().unwrap_or(0))};
    if slot(master,actor,minimum_duration).is_none(){return false;}
    actor.gekisou_skill.into_iter().filter_map(|(id,level)|master.gekiso_skills.get(&id).map(|skill|(skill,level)))
        .filter(|(skill,_)|skill.mission_type==4||chart.setup.missions.contains(&skill.mission_type))
        .flat_map(|(skill,level)|skill.effects.iter().filter(move|row|row.level==level))
        .all(|row|independent_row(master,row))
}
pub(super) fn scores(master:&Master,chart:&Chart,deck:&[Performer],seeds:&[i32],power:i32)->Result<Option<ReplayedScores>,String> {
    if deck.len()!=5||chart.luck&&seeds.is_empty()||master.live.life_onus<0.0||chart.range_percent.iter().any(|p|*p<0)||!chart.luck&&replay_random_skills(master,chart,deck) {return Ok(None);}
    let minimum_duration={let mut cache=chart.factored.lock().unwrap();2 * *cache.frame_ms.get_or_insert_with(||chart.play.frames.windows(2).map(|pair|pair[1].time_ms-pair[0].time_ms).max().unwrap_or(0))};
    let Some(slots)=deck.iter().map(|actor|slot(master,actor,minimum_duration)).collect::<Option<Vec<_>>>() else {return Ok(None)};
    for actor in deck {
        let rows=actor.gekisou_skill.into_iter().filter_map(|(id,level)|master.gekiso_skills.get(&id).map(|skill|(skill,level)))
            .chain(actor.gekisou_support_skills.iter().filter_map(|&(id,level)|master.gekiso_support_skills.get(&id).map(|skill|(skill,level))))
            .filter(|(skill,_)|skill.mission_type==4||chart.setup.missions.contains(&skill.mission_type))
            .flat_map(|(skill,level)|skill.effects.iter().filter(move|row|row.level==level));
        if rows.into_iter().any(|row|!independent_row(master,row)) {return Ok(None);}
    }
    let context=context(master,chart,deck,seeds)?;
    independent_orders(master,chart,&context,power,&slots)
}
